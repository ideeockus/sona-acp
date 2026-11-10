//! Which paths file tools may touch (D76).

use control_plane_common::layout::{SKILLS_DIR, WORKDIR};
use std::path::{Component, Path};

/// Pi's built-in file tools and where they may go. Other tools are not checked here:
/// `bash` is bounded by the container (D21), our own tools by their components.
fn roots(tool: &str) -> Option<&'static [&'static str]> {
    match tool {
        "write" | "edit" => Some(&[WORKDIR]),
        "read" | "grep" | "find" | "ls" => Some(&[WORKDIR, SKILLS_DIR]),
        _ => None,
    }
}

pub fn check(tool: &str, path: Option<&str>) -> Result<(), String> {
    let Some(roots) = roots(tool) else {
        return Ok(());
    };
    let path = path.ok_or_else(|| format!("{tool} needs a path"))?;
    let path = Path::new(path);

    // The extension resolves paths; anything else is refused rather than guessed at.
    let resolved = path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    if !resolved {
        return Err(format!(
            "{} is not a resolved absolute path",
            path.display()
        ));
    }

    // Component-wise, so `/workshop` is not inside `/work`.
    if roots.iter().any(|root| path.starts_with(root)) {
        Ok(())
    } else {
        Err(format!("{tool} may not access {}", path.display()))
    }
}
