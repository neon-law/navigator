//! Where a Project's repository lives on the installation's Forgejo.
//!
//! Each Firm is one Forgejo organization, named by its [`Firm::slug`], and
//! each Project it owns is one repository in that organization, named by its
//! [`Project::code`]: `<firm-slug>/<project-code>`. Both segments are
//! immutable, so a repository never moves when a Firm or Project is renamed.
//! [`repository_path`] is the one place that path is composed.

use std::fmt;

use uuid::Uuid;

use super::Firm;
use crate::projects::Project;

/// A Forgejo repository path: its owning organization and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryPath {
    /// The organization: the owning Firm's slug.
    pub owner: String,
    /// The repository: the Project's code.
    pub repository: String,
}

impl fmt::Display for RepositoryPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repository)
    }
}

/// Why a Project has no repository path under a Firm.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryPathError {
    /// The Project is not owned by this Firm (or by any Firm yet).
    #[error("project {project_id} is not owned by firm {firm_id}")]
    NotOwned { project_id: Uuid, firm_id: Uuid },
}

/// The Forgejo path of `project`'s repository: `<firm-slug>/<project-code>`.
///
/// Refuses a Project that `firm` does not own, so a matter's repository can
/// never be composed into another practice's organization.
pub fn repository_path(
    firm: &Firm,
    project: &Project,
) -> Result<RepositoryPath, RepositoryPathError> {
    if project.firm_id != Some(firm.id) {
        return Err(RepositoryPathError::NotOwned {
            project_id: project.id,
            firm_id: firm.id,
        });
    }
    Ok(RepositoryPath {
        owner: firm.slug.clone(),
        repository: project.code.clone(),
    })
}
