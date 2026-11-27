//! The guard that keeps one Google Cloud project from being provisioned as
//! two different things.
//!
//! An installation has one **hub** that holds container images and the
//! **environments** that actually run Navigator. The operator names the
//! environment projects in [`NAVIGATOR_ENVIRONMENT_PROJECT_IDS`]; none is
//! named in source.
//!
//! The provisioners are separate commands with very different blast radii —
//! `ops gcp hub setup` creates a registry and an identity, `ops gcp setup`
//! creates buckets and a GKE cluster. Pointing either at the
//! other's project would provision resources into a project whose whole point
//! is not to hold them. [`validate_target`] refuses that before the first GCP
//! call, and [`validate_images_project`] refuses an environment that names
//! itself as its own image hub.
//!
//! An unrecorded project ID is always allowed: the guard only knows the
//! projects the operator recorded.

use super::error::{SetupError, SetupResult};

/// The shared image hub. Not an environment — nothing runs there.
pub const HUB_PROJECT_ID: &str = "ghcr";

/// Environment key listing the runtime projects, comma-separated, in release
/// order.
pub const NAVIGATOR_ENVIRONMENT_PROJECT_IDS: &str = "NAVIGATOR_ENVIRONMENT_PROJECT_IDS";

/// Deprecated: the environments recorded when
/// [`NAVIGATOR_ENVIRONMENT_PROJECT_IDS`] is unset.
const DEPRECATED_ENVIRONMENT_PROJECT_IDS: &[&str] = &["neon-law-stg", "neon-law"];

/// The recorded environment projects: `configured` split on commas, or the
/// deprecated default when it is unset or names none.
#[must_use]
pub fn environment_project_ids(configured: Option<&str>) -> Vec<String> {
    let named: Vec<String> = configured
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    if named.is_empty() {
        DEPRECATED_ENVIRONMENT_PROJECT_IDS
            .iter()
            .map(ToString::to_string)
            .collect()
    } else {
        named
    }
}

fn recorded_environments() -> Vec<String> {
    environment_project_ids(
        std::env::var(NAVIGATOR_ENVIRONMENT_PROJECT_IDS)
            .ok()
            .as_deref(),
    )
}

/// What a project ID is being provisioned as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantRole {
    /// `ops gcp hub setup` — the shared registry project.
    Hub,
    /// `ops gcp setup` — one environment.
    Environment,
    /// A separate project for locked archives.
    Archive,
}

impl TenantRole {
    /// Human-readable form used in [`SetupError::TenantConflict`].
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hub => "the image hub",
            Self::Environment => "an environment",
            Self::Archive => "a separate archive project",
        }
    }
}

/// The role `project_id` is recorded under among `environments` and the hub, or
/// `None` when the workspace does not know it.
fn recorded_role_among(project_id: &str, environments: &[String]) -> Option<TenantRole> {
    let project_id = project_id.trim();
    if project_id == HUB_PROJECT_ID {
        return Some(TenantRole::Hub);
    }
    environments
        .iter()
        .any(|id| id == project_id)
        .then_some(TenantRole::Environment)
}

/// Refuse `project_id` when it is recorded for a target other than `role`.
///
/// The successor to `staging::StagingSetupConfig::validate`'s
/// `staging != production` check, generalized from two targets to four.
pub fn validate_target(role: TenantRole, project_id: &str) -> SetupResult<()> {
    validate_target_among(role, project_id, &recorded_environments())
}

fn validate_target_among(
    role: TenantRole,
    project_id: &str,
    environments: &[String],
) -> SetupResult<()> {
    if project_id.trim().is_empty() {
        return Err(SetupError::MissingConfiguration("--project-id"));
    }
    match recorded_role_among(project_id, environments) {
        Some(recorded) if recorded != role => Err(SetupError::TenantConflict {
            project_id: project_id.trim().to_string(),
            recorded: recorded.label(),
            requested: role.label(),
        }),
        _ => Ok(()),
    }
}

/// Refuse an `--images-project-id` that is not a usable hub for
/// `environment_project_id`: empty, the environment itself, or a project
/// recorded as an environment.
pub fn validate_images_project(
    environment_project_id: &str,
    images_project_id: &str,
) -> SetupResult<()> {
    validate_images_project_among(
        environment_project_id,
        images_project_id,
        &recorded_environments(),
    )
}

fn validate_images_project_among(
    environment_project_id: &str,
    images_project_id: &str,
    environments: &[String],
) -> SetupResult<()> {
    if images_project_id.trim().is_empty() {
        return Err(SetupError::MissingConfiguration("--images-project-id"));
    }
    if images_project_id.trim() == environment_project_id.trim() {
        return Err(SetupError::TenantConflict {
            project_id: images_project_id.trim().to_string(),
            recorded: TenantRole::Environment.label(),
            requested: TenantRole::Hub.label(),
        });
    }
    validate_target_among(TenantRole::Hub, images_project_id, environments)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invented environment projects, as an operator would record them.
    fn environments() -> Vec<String> {
        environment_project_ids(Some("example-staging, example-production"))
    }

    #[test]
    fn the_recorded_environments_come_from_configuration() {
        assert_eq!(environments(), ["example-staging", "example-production"]);
        // Unset, blank, and comma-only all name none: the deprecated default.
        let default = environment_project_ids(None);
        assert!(!default.is_empty());
        assert_eq!(environment_project_ids(Some(" ")), default);
        assert_eq!(environment_project_ids(Some(" , ")), default);
        assert!(!default.iter().any(|id| id == HUB_PROJECT_ID));
    }

    #[test]
    fn the_hub_command_refuses_an_environment_project() {
        for project_id in &environments() {
            let err = validate_target_among(TenantRole::Hub, project_id, &environments())
                .expect_err("an environment is not the hub");
            let message = err.to_string();
            assert!(message.contains(project_id.as_str()), "{message}");
            assert!(message.contains("an environment"), "{message}");
            assert!(message.contains("the image hub"), "{message}");
        }
    }

    #[test]
    fn the_environment_command_refuses_the_hub_project() {
        let err = validate_target_among(TenantRole::Environment, HUB_PROJECT_ID, &environments())
            .expect_err("the hub must never receive buckets or GKE");
        let message = err.to_string();
        assert!(message.contains(HUB_PROJECT_ID), "{message}");
        assert!(message.contains("the image hub"), "{message}");
    }

    #[test]
    fn each_recorded_project_is_accepted_for_its_own_role() {
        validate_target_among(TenantRole::Hub, HUB_PROJECT_ID, &environments()).unwrap();
        for project_id in &environments() {
            validate_target_among(TenantRole::Environment, project_id, &environments()).unwrap();
        }
    }

    #[test]
    fn an_unrecorded_project_is_allowed_for_either_role() {
        validate_target_among(TenantRole::Hub, "a-fork-registry", &environments()).unwrap();
        validate_target_among(TenantRole::Environment, "a-fork-prod", &environments()).unwrap();
    }

    #[test]
    fn an_empty_project_id_is_refused_before_any_gcp_call() {
        let err = validate_target_among(TenantRole::Hub, "   ", &environments()).unwrap_err();
        assert!(err.to_string().contains("--project-id"), "{err}");
    }

    #[test]
    fn an_environment_may_not_be_its_own_image_hub() {
        let err = validate_images_project_among(
            "example-production",
            "example-production",
            &environments(),
        )
        .expect_err("an environment never hosts the shared registry");
        assert!(err.to_string().contains("example-production"), "{err}");
    }

    #[test]
    fn images_project_must_not_be_another_environment() {
        let err =
            validate_images_project_among("example-production", "example-staging", &environments())
                .expect_err("images come from the hub, never from a peer environment");
        assert!(err.to_string().contains("example-staging"), "{err}");
    }

    #[test]
    fn the_hub_is_a_valid_images_project_for_every_environment() {
        for project_id in &environments() {
            validate_images_project_among(project_id, HUB_PROJECT_ID, &environments()).unwrap();
        }
    }

    #[test]
    fn an_empty_images_project_names_its_own_flag() {
        let err = validate_images_project("example-production", "").unwrap_err();
        assert!(err.to_string().contains("--images-project-id"), "{err}");
    }
}
