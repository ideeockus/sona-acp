//! A skill's files, read from a directory, checked, hashed and kept as a version.

use control_plane_common::skills::{MAX_SKILL_BYTES, MAX_SKILL_FILES, SkillFile};
use control_plane_common::{Error, Result};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

/// A skill as read from disk: its files in path order and what `SKILL.md` says.
pub struct Snapshot {
    pub name: String,
    pub description: String,
    pub version: String,
    pub files: Vec<(String, Vec<u8>)>,
}

impl Snapshot {
    /// Reads the skill in `dir`. Symlinks are refused: one could point anywhere on the
    /// host, and the snapshot is mounted into agents.
    pub async fn read(dir: &Path) -> Result<Self> {
        let mut files = Vec::new();
        let mut total = 0usize;
        let mut pending = vec![PathBuf::new()];
        while let Some(relative) = pending.pop() {
            let mut entries = tokio::fs::read_dir(dir.join(&relative)).await.map_err(io)?;
            while let Some(entry) = entries.next_entry().await.map_err(io)? {
                let name = entry.file_name();
                if name == ".git" {
                    continue;
                }
                let path = relative.join(&name);
                let kind = entry.file_type().await.map_err(io)?;
                if kind.is_symlink() {
                    return Err(invalid(format!("{} is a symlink", path.display())));
                } else if kind.is_dir() {
                    pending.push(path);
                } else if kind.is_file() {
                    let content = tokio::fs::read(entry.path()).await.map_err(io)?;
                    total += content.len();
                    files.push((portable(&path)?, content));
                    if files.len() > MAX_SKILL_FILES || total > MAX_SKILL_BYTES {
                        return Err(invalid(format!(
                            "a skill holds at most {MAX_SKILL_FILES} files and {} MiB",
                            MAX_SKILL_BYTES / 1024 / 1024
                        )));
                    }
                }
            }
        }
        Self::of(files)
    }

    /// Checks files given as path and content, e.g. those an agent proposed.
    pub fn of(mut files: Vec<(String, Vec<u8>)>) -> Result<Self> {
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let total: usize = files.iter().map(|(_, c)| c.len()).sum();
        if files.len() > MAX_SKILL_FILES || total > MAX_SKILL_BYTES {
            return Err(invalid(format!(
                "a skill holds at most {MAX_SKILL_FILES} files and {} MiB",
                MAX_SKILL_BYTES / 1024 / 1024
            )));
        }
        for (path, _) in &files {
            check_path(path)?;
        }
        let manifest = files
            .iter()
            .find(|(path, _)| path == "SKILL.md")
            .ok_or_else(|| invalid("a skill needs SKILL.md at its root".to_owned()))?;
        let text = std::str::from_utf8(&manifest.1)
            .map_err(|_| invalid("SKILL.md is not UTF-8".to_owned()))?;
        let (name, description) = frontmatter(text)?;

        let mut hash = Sha256::new();
        for (path, content) in &files {
            hash.update((path.len() as u64).to_le_bytes());
            hash.update(path.as_bytes());
            hash.update((content.len() as u64).to_le_bytes());
            hash.update(content);
        }
        let version = hash
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Self {
            name,
            description,
            version,
            files,
        })
    }

    /// Writes the version under `root/<name>/<version>/`, unless it is there already.
    pub async fn store(&self, root: &Path) -> Result<PathBuf> {
        let dir = root.join(&self.name).join(&self.version);
        if tokio::fs::try_exists(&dir).await.map_err(io)? {
            return Ok(dir);
        }
        // Into a sibling first, then renamed: a half-written version is never seen.
        let partial = root
            .join(&self.name)
            .join(format!(".{}.partial", self.version));
        let _ = tokio::fs::remove_dir_all(&partial).await;
        for (path, content) in &self.files {
            let target = partial.join(path);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(io)?;
            }
            tokio::fs::write(&target, content).await.map_err(io)?;
        }
        tokio::fs::rename(&partial, &dir).await.map_err(io)?;
        Ok(dir)
    }

    pub fn listing(&self) -> Vec<SkillFile> {
        self.files
            .iter()
            .map(|(path, content)| SkillFile {
                path: path.clone(),
                size: content.len() as u64,
            })
            .collect()
    }
}

/// A relative path with only normal parts, `/`-separated.
fn portable(path: &Path) -> Result<String> {
    let parts: Option<Vec<&str>> = path
        .components()
        .map(|c| match c {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    parts
        .map(|parts| parts.join("/"))
        .ok_or_else(|| invalid(format!("{} is not a plain relative path", path.display())))
}

fn check_path(path: &str) -> Result<()> {
    let plain = !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'));
    if plain {
        Ok(())
    } else {
        Err(invalid(format!("{path:?} is not a plain relative path")))
    }
}

/// `name` and `description` from the YAML frontmatter, as the Agent Skills
/// specification defines them. Only what those two keys need: plain, quoted and
/// block (`>` or `|`) values.
pub fn frontmatter(text: &str) -> Result<(String, String)> {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err(invalid(
            "SKILL.md must start with --- frontmatter".to_owned(),
        ));
    }
    let block: Vec<&str> = lines.take_while(|l| l.trim_end() != "---").collect();
    let value = |key: &str| -> Option<String> {
        let at = block.iter().position(|l| {
            l.strip_prefix(key)
                .is_some_and(|rest| rest.starts_with(':'))
        })?;
        let first = block[at][key.len() + 1..].trim();
        if first.is_empty() || first.starts_with('>') || first.starts_with('|') {
            let folded = !first.starts_with('|');
            let parts: Vec<&str> = block[at + 1..]
                .iter()
                .take_while(|l| l.starts_with(' ') || l.starts_with('\t') || l.trim().is_empty())
                .map(|l| l.trim())
                .collect();
            let joined = parts.join(if folded { " " } else { "\n" });
            return Some(joined.trim().to_owned());
        }
        let unquoted = first
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| first.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(first);
        Some(unquoted.to_owned())
    };
    let name = value("name").ok_or_else(|| invalid("SKILL.md has no name".to_owned()))?;
    let description =
        value("description").ok_or_else(|| invalid("SKILL.md has no description".to_owned()))?;
    let valid_name = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if !valid_name {
        return Err(invalid(format!(
            "{name:?} is not a skill name: lowercase letters, digits and single hyphens, up to 64"
        )));
    }
    if description.is_empty() || description.chars().count() > 1024 {
        return Err(invalid("a description is 1 to 1024 characters".to_owned()));
    }
    Ok((name, description))
}

fn invalid(message: String) -> Error {
    Error::InvalidInput(message)
}

fn io(error: std::io::Error) -> Error {
    Error::Internal(error.to_string())
}
