//! The forge a deployment creates Project repositories on, and how talking to
//! it fails.

use thiserror::Error;

use crate::workspace::WorkspaceConfigError;

#[derive(Debug, Error)]
pub enum ForgeError {
    #[error("missing required forge configuration: {0}")]
    MissingConfig(&'static str),
    #[error("invalid forge configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("resolve the deployment forge pair: {0}")]
    Workspace(#[from] WorkspaceConfigError),
    #[error("forge authentication failed")]
    Authentication,
    #[error("could not authenticate with the configured GitHub App")]
    GitHubAppAuthentication,
    #[error("GitHub did not return a usable installation access token")]
    MissingInstallationToken,
    #[error("forge request failed while {action}")]
    Request {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("forge API returned {status} while {action}")]
    Api { action: &'static str, status: u16 },
    #[error("forge API returned an invalid response while {action}")]
    Response {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("forge API response while {action} did not include a repository URL")]
    MissingUrl { action: &'static str },
    #[error(
        "repository {name} is not private after provisioning; a policy regression or a \
         deliberate visibility change happened underneath, and both need a human, not a retry"
    )]
    NotPrivate { name: String },
    #[error("Project repository {name} does not exist in the configured GitHub organization")]
    ProjectRepositoryMissing { name: String },
    #[error(
        "repository {name} is not this Firm's repository for the Project; it is never adopted"
    )]
    ForeignRepository { name: String },
    #[error(
        "repository {name} already holds source that is not this import; it is never replaced \
         implicitly"
    )]
    RepositoryNotEmpty { name: String },
    #[error("this forge cannot {0}")]
    Unsupported(&'static str),
}

/// Selects the deployment's Project forge: `github` (the default) or
/// `forgejo`.
pub const NAVIGATOR_FORGE_BACKEND_ENV: &str = "NAVIGATOR_FORGE_BACKEND";

/// Which forge creates a deployment's Project repositories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeBackend {
    GitHub,
    Forgejo,
}

impl ForgeBackend {
    /// Read [`NAVIGATOR_FORGE_BACKEND_ENV`]. Unset is GitHub, the existing
    /// deployments' forge; any other value than the two names fails closed.
    ///
    /// # Errors
    /// [`ForgeError::InvalidConfig`] for an unknown value.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ForgeError> {
        match get(NAVIGATOR_FORGE_BACKEND_ENV).as_deref().map(str::trim) {
            None | Some("" | "github") => Ok(Self::GitHub),
            Some("forgejo") => Ok(Self::Forgejo),
            Some(_) => Err(ForgeError::InvalidConfig(NAVIGATOR_FORGE_BACKEND_ENV)),
        }
    }
}
