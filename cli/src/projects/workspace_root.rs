//! Resolve the lawyer's local root for Project checkouts.

use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};

/// Select an explicitly requested root, the sole existing legacy/new root,
/// or `~/Projects` for a fresh installation. Existing roots are never moved.
pub fn resolve(home: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
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

    let work = home.join("Work");
    let projects = home.join("Projects");
    let has_work = exists(&work)?;
    let has_projects = exists(&projects)?;
    match (has_work, has_projects) {
        (true, true) => bail!(
            "both ~/Work and ~/Projects exist; choose the Project checkout root explicitly with --dir <path>"
        ),
        (true, false) => Ok(work),
        (false, true) | (false, false) => Ok(projects),
    }
}

pub fn selected(explicit: Option<&Path>) -> Result<PathBuf> {
    let home =
        std::env::home_dir().context("cannot locate the home directory for Project checkouts")?;
    resolve(&home, explicit)
}

fn exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(metadata.is_dir(), "{} is not a directory", path.display());
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_home_uses_projects_without_creating_it() {
        let home = tempfile::tempdir().unwrap();
        let root = resolve(home.path(), None).unwrap();
        assert_eq!(root, home.path().join("Projects"));
        assert!(!root.exists());
    }

    #[test]
    fn existing_work_is_preserved_as_the_root() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("Work")).unwrap();
        assert_eq!(
            resolve(home.path(), None).unwrap(),
            home.path().join("Work")
        );
    }

    #[test]
    fn both_roots_require_an_explicit_choice() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("Work")).unwrap();
        std::fs::create_dir(home.path().join("Projects")).unwrap();
        let error = resolve(home.path(), None).unwrap_err().to_string();
        assert!(error.contains("both ~/Work and ~/Projects exist"));
        assert!(error.contains("--dir"));
        assert_eq!(
            resolve(home.path(), Some(Path::new("Projects"))).unwrap(),
            std::env::current_dir().unwrap().join("Projects")
        );
    }

    #[test]
    fn explicit_selection_wins_for_either_existing_root() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("Work")).unwrap();
        std::fs::create_dir(home.path().join("Projects")).unwrap();
        for name in ["Work", "Projects"] {
            let selected = resolve(home.path(), Some(&home.path().join(name))).unwrap();
            assert_eq!(selected, home.path().join(name));
        }
    }
}
