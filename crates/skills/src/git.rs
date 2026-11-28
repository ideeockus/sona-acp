//! Fetching one commit of a repository with the `git` CLI, as the host has it (D109).
//!
//! The repository is someone else's: no prompt, no credentials, no user or system
//! config (and so no credential helper), only `https://`, one commit and no history.

use control_plane_common::{Error, Result};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Checks out `git_ref` of `url` into `dir`, which must be empty or missing, and
/// returns the commit it was.
pub async fn fetch(url: &str, git_ref: &str, dir: &Path) -> Result<String> {
    if !url.starts_with("https://") || url.contains(char::is_whitespace) {
        return Err(Error::InvalidInput(format!(
            "{url:?} is not an https:// repository URL"
        )));
    }
    if git_ref.starts_with('-') || git_ref.contains(char::is_whitespace) {
        return Err(Error::InvalidInput(format!("{git_ref:?} is not a git ref")));
    }
    tokio::fs::create_dir_all(dir).await.map_err(io)?;
    git(dir, &["init", "-q"]).await?;
    git(
        dir,
        &["fetch", "-q", "--depth", "1", "--no-tags", url, git_ref],
    )
    .await?;
    git(dir, &["checkout", "-q", "--detach", "FETCH_HEAD"]).await?;
    let commit = git(dir, &["rev-parse", "HEAD"]).await?;
    Ok(commit.trim().to_owned())
}

async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let run = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.symlinks=false",
        ])
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(FETCH_TIMEOUT, run)
        .await
        .map_err(|_| Error::InvalidInput("git took too long".to_owned()))?
        .map_err(|e| Error::Internal(format!("cannot run git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::InvalidInput(format!(
            "git {}: {}",
            args[0],
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn io(error: std::io::Error) -> Error {
    Error::Internal(error.to_string())
}
