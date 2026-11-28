//! The skill library (D109): skills from git or proposed by agents, kept as immutable
//! versions under `data_dir/skills`, enabled by the operator after review.

mod git;
mod snapshot;

use async_trait::async_trait;
use control_plane_common::skills::{
    GitSkill, ProposedFile, Skill, SkillDetail, SkillLibrary, SkillMounts, SkillSource,
};
use control_plane_common::{Alias, Caller, Error, Result, Timestamp};
use snapshot::Snapshot;
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const COLUMNS: &str = "name, description, source, version, enabled, added_by, added_at, updated_at";

pub struct SqliteSkills {
    pool: SqlitePool,
    /// `data_dir/skills`: a directory per skill, a subdirectory per version.
    root: PathBuf,
}

impl SqliteSkills {
    pub async fn new(pool: SqlitePool, root: PathBuf) -> Result<Self> {
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?;
        tokio::fs::create_dir_all(&root).await.map_err(io)?;
        Ok(Self { pool, root })
    }

    async fn find(&self, name: &str) -> Result<Option<Skill>> {
        let row: Option<Row> =
            sqlx::query_as(&format!("SELECT {COLUMNS} FROM skills WHERE name = ?"))
                .bind(name)
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        row.map(Row::into_skill).transpose()
    }

    async fn get(&self, name: &str) -> Result<Skill> {
        self.find(name)
            .await?
            .ok_or_else(|| Error::NotFound(format!("skill {name}")))
    }

    /// Fetches the skill at `source` into a scratch directory and reads it.
    async fn fetch(&self, source: &GitSkill) -> Result<(Snapshot, String)> {
        let scratch = self.root.join(format!(".fetch-{}", uuid::Uuid::now_v7()));
        let fetched = async {
            let git_ref = source.git_ref.as_deref().unwrap_or("HEAD");
            let commit = git::fetch(&source.url, git_ref, &scratch).await?;
            let dir = match source
                .path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
            {
                Some(path) => {
                    let relative = Path::new(path.trim_matches('/'));
                    if relative
                        .components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
                    {
                        return Err(Error::InvalidInput(format!(
                            "{path:?} is not a path in the repository"
                        )));
                    }
                    scratch.join(relative)
                }
                None => scratch.clone(),
            };
            Ok((Snapshot::read(&dir).await?, commit))
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&scratch).await;
        fetched
    }

    /// Stores a new version and writes the row, disabled: new content is reviewed first.
    async fn save(
        &self,
        snapshot: &Snapshot,
        source: &SkillSource,
        by: &Alias,
        new: bool,
    ) -> Result<Skill> {
        snapshot.store(&self.root).await?;
        let now = Timestamp::now().as_unix_millis();
        let source = serde_json::to_string(source).map_err(|e| Error::Internal(e.to_string()))?;
        if new {
            sqlx::query(
                "INSERT INTO skills (name, description, source, version, enabled, added_by, added_at, updated_at)
                 VALUES (?, ?, ?, ?, 0, ?, ?, ?)",
            )
            .bind(&snapshot.name)
            .bind(&snapshot.description)
            .bind(&source)
            .bind(&snapshot.version)
            .bind(by.as_str())
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        } else {
            sqlx::query(
                "UPDATE skills SET description = ?, source = ?, version = ?, enabled = 0, updated_at = ?
                 WHERE name = ?",
            )
            .bind(&snapshot.description)
            .bind(&source)
            .bind(&snapshot.version)
            .bind(now)
            .bind(&snapshot.name)
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        }
        self.get(&snapshot.name).await
    }
}

#[async_trait]
impl SkillLibrary for SqliteSkills {
    async fn list_skills(&self, caller: &Caller) -> Result<Vec<Skill>> {
        require_operator(caller)?;
        let rows: Vec<Row> = sqlx::query_as(&format!("SELECT {COLUMNS} FROM skills ORDER BY name"))
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?;
        rows.into_iter().map(Row::into_skill).collect()
    }

    async fn get_skill(&self, caller: &Caller, name: &str) -> Result<SkillDetail> {
        require_operator(caller)?;
        let skill = self.get(name).await?;
        let snapshot = Snapshot::read(&self.root.join(&skill.name).join(&skill.version)).await?;
        let instructions = snapshot
            .files
            .iter()
            .find(|(path, _)| path == "SKILL.md")
            .map(|(_, content)| String::from_utf8_lossy(content).into_owned())
            .unwrap_or_default();
        Ok(SkillDetail {
            files: snapshot.listing(),
            skill,
            instructions,
        })
    }

    async fn add_from_git(&self, caller: &Caller, source: GitSkill) -> Result<Skill> {
        require_operator(caller)?;
        let (snapshot, commit) = self.fetch(&source).await?;
        if self.find(&snapshot.name).await?.is_some() {
            return Err(Error::Conflict(format!(
                "the library already has a skill {}",
                snapshot.name
            )));
        }
        let origin = SkillSource::Git {
            url: source.url,
            git_ref: source.git_ref.unwrap_or_else(|| "HEAD".to_owned()),
            path: source.path.unwrap_or_default(),
            commit,
        };
        self.save(&snapshot, &origin, caller.alias(), true).await
    }

    async fn update_skill(&self, caller: &Caller, name: &str) -> Result<Skill> {
        require_operator(caller)?;
        let skill = self.get(name).await?;
        let SkillSource::Git {
            url, git_ref, path, ..
        } = &skill.source
        else {
            return Err(Error::InvalidInput(format!(
                "{name} was proposed by an agent: it changes when the agent proposes it again"
            )));
        };
        let source = GitSkill {
            url: url.clone(),
            git_ref: Some(git_ref.clone()),
            path: Some(path.clone()),
        };
        let (snapshot, commit) = self.fetch(&source).await?;
        if snapshot.name != skill.name {
            return Err(Error::InvalidInput(format!(
                "the skill there is now named {}, not {name}",
                snapshot.name
            )));
        }
        if snapshot.version == skill.version {
            return Ok(skill);
        }
        let origin = SkillSource::Git {
            url: url.clone(),
            git_ref: git_ref.clone(),
            path: path.clone(),
            commit,
        };
        self.save(&snapshot, &origin, caller.alias(), false).await
    }

    async fn set_enabled(&self, caller: &Caller, name: &str, enabled: bool) -> Result<Skill> {
        require_operator(caller)?;
        let updated = sqlx::query("UPDATE skills SET enabled = ? WHERE name = ?")
            .bind(enabled)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        if updated.rows_affected() == 0 {
            return Err(Error::NotFound(format!("skill {name}")));
        }
        self.get(name).await
    }

    async fn delete_skill(&self, caller: &Caller, name: &str) -> Result<()> {
        require_operator(caller)?;
        let deleted = sqlx::query("DELETE FROM skills WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        if deleted.rows_affected() == 0 {
            return Err(Error::NotFound(format!("skill {name}")));
        }
        // A Run started before keeps its mount; the next one has nothing to mount.
        let _ = tokio::fs::remove_dir_all(self.root.join(name)).await;
        Ok(())
    }

    async fn propose(&self, caller: &Caller, files: Vec<ProposedFile>) -> Result<Skill> {
        let Caller::Agent { alias, .. } = caller else {
            return Err(Error::Forbidden("only an agent proposes skills".to_owned()));
        };
        let snapshot = Snapshot::of(
            files
                .into_iter()
                .map(|f| (f.path, f.content.into_bytes()))
                .collect(),
        )?;
        let source = SkillSource::Proposed { by: alias.clone() };
        match self.find(&snapshot.name).await? {
            None => self.save(&snapshot, &source, alias, true).await,
            Some(existing) if existing.source == source => {
                if existing.version == snapshot.version {
                    Ok(existing)
                } else {
                    self.save(&snapshot, &source, alias, false).await
                }
            }
            Some(_) => Err(Error::Conflict(format!(
                "the library already has a skill {}: choose another name",
                snapshot.name
            ))),
        }
    }
}

#[async_trait]
impl SkillMounts for SqliteSkills {
    async fn enabled(&self, names: &[String]) -> Result<Vec<(String, PathBuf)>> {
        let mut mounts = Vec::new();
        for name in names {
            if let Some(skill) = self.find(name).await?
                && skill.enabled
            {
                mounts.push((
                    skill.name.clone(),
                    self.root.join(&skill.name).join(&skill.version),
                ));
            }
        }
        Ok(mounts)
    }
}

fn require_operator(caller: &Caller) -> Result<()> {
    match caller {
        Caller::Operator { .. } => Ok(()),
        other => Err(Error::Forbidden(format!(
            "{} is not the operator",
            other.alias()
        ))),
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    name: String,
    description: String,
    source: String,
    version: String,
    enabled: bool,
    added_by: String,
    added_at: i64,
    updated_at: i64,
}

impl Row {
    fn into_skill(self) -> Result<Skill> {
        Ok(Skill {
            name: self.name,
            description: self.description,
            source: serde_json::from_str(&self.source)
                .map_err(|e| Error::Internal(e.to_string()))?,
            version: self.version,
            enabled: self.enabled,
            added_by: parse(&self.added_by)?,
            added_at: Timestamp::from_unix_millis(self.added_at)?,
            updated_at: Timestamp::from_unix_millis(self.updated_at)?,
        })
    }
}

fn internal(error: sqlx::Error) -> Error {
    Error::Internal(error.to_string())
}

fn io(error: std::io::Error) -> Error {
    Error::Internal(error.to_string())
}

fn parse<T: FromStr>(value: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|e: T::Err| Error::Internal(e.to_string()))
}

#[cfg(test)]
mod tests;
