//! Resolve the lawyer's local root for Project checkouts.

use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};

pub const WORKSPACE_ROOT_ENV: &str = "NAVIGATOR_WORKSPACE_ROOT";

/// Select an explicit root or the configured root, defaulting to `~/Projects`.
pub fn resolve(home: &Path, explicit: Option<&Path>, configured: Option<&Path>) -> Result<PathBuf> {
    ensure!(
        home.is_absolute(),
        "home directory must be an absolute path"
    );
    if let Some(path) = explicit {
        return Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        });
    }

    let selected = configured
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("Projects"));
    Ok(if selected.is_absolute() {
        selected.to_path_buf()
    } else {
        home.join(selected)
    })
}

pub fn selected(explicit: Option<&Path>) -> Result<PathBuf> {
    let home =
        std::env::home_dir().context("cannot locate the home directory for Project checkouts")?;
    let configured = std::env::var_os(WORKSPACE_ROOT_ENV).map(PathBuf::from);
    resolve(&home, explicit, configured.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_home_uses_projects_without_creating_it() {
        let home = tempfile::tempdir().unwrap();
        let root = resolve(home.path(), None, None).unwrap();
        assert_eq!(root, home.path().join("Projects"));
        assert!(!root.exists());
    }

    #[test]
    fn configured_relative_root_is_resolved_from_home() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve(home.path(), None, Some(Path::new("Work"))).unwrap(),
            home.path().join("Work")
        );
    }

    #[test]
    fn configured_absolute_root_is_used_directly() {
        let home = tempfile::tempdir().unwrap();
        let configured = home.path().join("custom");
        assert_eq!(
            resolve(home.path(), None, Some(&configured)).unwrap(),
            configured
        );
    }

    #[test]
    fn explicit_relative_root_overrides_configuration() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve(home.path(), Some(Path::new(".")), Some(Path::new("custom"))).unwrap(),
            std::env::current_dir().unwrap().join(".")
        );
    }

    #[test]
    fn default_does_not_depend_on_existing_directories() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("Work")).unwrap();
        std::fs::create_dir(home.path().join("Projects")).unwrap();
        assert_eq!(
            resolve(home.path(), None, None).unwrap(),
            home.path().join("Projects")
        );
    }
}
