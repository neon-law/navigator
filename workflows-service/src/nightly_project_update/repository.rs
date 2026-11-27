//! The `repository` tag: one open Project's private repository is created or
//! adopted and recorded on the row, then its committed `navigator.yaml` pin
//! is read and compared against the running release. Participation is never
//! copied onto the forge. A closed or archived Project is never given a
//! repository, and a deployment with no forge skips the tag.
//!
//! On Forgejo the tag works through the Project's own Firm client and, once
//! the repository has a head commit, also
//! keeps it governed and current: it reconciles the repository governance
//! (squash merge only, `main` protected, the `navigator project gate` check,
//! signed commits, no administrator exemption) and, for a stale pin, proposes
//! the one-line `version:` bump on `navigator/update-<release>` as a pull
//! request with squash auto-merge. Nothing is pushed to `main`, and a re-run
//! finds its own pull request rather than opening a second one. On GitHub the
//! pin is still detection only.
//!
//! One unreadable manifest is counted, not fatal. The outcome carries
//! versions, counts, and statuses — never a repository URL or a Project name.

use std::sync::Arc;

use cloud::forge::ForgeService;
use cloud::forgejo_governance::{FileUpdate, GovernedForge, UpdateProposal};
use cloud::ForgeBackend;

use rules::project_manifest::{
    pin_status, pinned_version, with_pinned_version, PinStatus, FILE as MANIFEST,
};
use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use store::project_surfaces::SurfaceStatus;
use uuid::Uuid;

use crate::workflow_step_error::WorkflowStepError;

/// The release this worker is: what every repository's pin is compared to.
pub const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");

/// What one Project's `repository` child did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RepositoryOutcome {
    /// `1` when the repository was created or adopted this run.
    pub synced: usize,
    /// How the repository's `navigator.yaml` pin stands against the running
    /// release; `None` when no forge is configured or the Project is closed.
    pub pin: Option<PinStatus>,
    /// The `version:` the repository pins, when the manifest was readable.
    pub pinned_version: Option<String>,
    /// The release that read it.
    pub runtime_version: Option<String>,
    /// How many readable governance controls had drifted and were repaired.
    #[serde(default)]
    pub governance_repaired: usize,
    /// What the version update did for a stale pin; `None` when the pin was
    /// not stale or the forge proposes no updates.
    #[serde(default)]
    pub update: Option<UpdateProposal>,
}

/// The forge each Project's `repository` child works through.
#[derive(Clone, Default)]
pub enum RepositoryForge {
    /// No forge is configured: the tag reports nothing.
    #[default]
    None,
    /// One deployment-wide GitHub client. The pin is read, never written.
    GitHub(Arc<dyn ForgeService>),
    /// Forgejo: each Project's own Firm client, which also governs the
    /// repository and proposes the version update.
    Forgejo(Arc<dyn FirmForges>),
    /// Route recorded repositories by origin. The deployment default is
    /// used only when a Project has no recorded repository.
    ByProject {
        default: ForgeBackend,
        github: Option<Arc<dyn ForgeService>>,
        forgejo: Arc<dyn FirmForges>,
        forgejo_origin: Option<url::Origin>,
    },
}

impl RepositoryForge {
    pub(super) fn for_repository(&self, repository_url: Option<&str>) -> Self {
        let Self::ByProject {
            default,
            github,
            forgejo,
            forgejo_origin,
        } = self
        else {
            return self.clone();
        };
        let backend = match repository_url {
            None => *default,
            Some(recorded) => match url::Url::parse(recorded) {
                Ok(url) if forgejo_origin.as_ref() == Some(&url.origin()) => ForgeBackend::Forgejo,
                Ok(url) if url.origin().ascii_serialization() == "https://github.com" => {
                    ForgeBackend::GitHub
                }
                _ => return Self::None,
            },
        };
        match backend {
            ForgeBackend::Forgejo => Self::Forgejo(Arc::clone(forgejo)),
            ForgeBackend::GitHub => github.clone().map_or(Self::None, Self::GitHub),
        }
    }
}

pub(super) fn matches_recorded_repository(found: &str, recorded: &str) -> bool {
    found.trim_end_matches('/') == recorded.trim_end_matches('/')
}

/// Resolves the Forgejo client confined to one Project's Firm.
#[async_trait::async_trait]
pub trait FirmForges: Send + Sync {
    /// # Errors
    /// When the Firm, its token, or the Forgejo root cannot be resolved.
    async fn for_project(
        &self,
        application_store: &ApplicationStore,
        project_id: Uuid,
    ) -> Result<Arc<dyn GovernedForge>, WorkflowStepError>;
}

/// [`FirmForges`] from the process environment and the runtime KMS that
/// opens each Firm's encrypted `forgejo_access_token`.
pub struct EnvFirmForges {
    kms: Arc<dyn cloud::RuntimeKms>,
}

impl EnvFirmForges {
    #[must_use]
    pub fn new(kms: Arc<dyn cloud::RuntimeKms>) -> Self {
        Self { kms }
    }
}

#[async_trait::async_trait]
impl FirmForges for EnvFirmForges {
    async fn for_project(
        &self,
        application_store: &ApplicationStore,
        project_id: Uuid,
    ) -> Result<Arc<dyn GovernedForge>, WorkflowStepError> {
        let forge = store::project_surfaces::forgejo_for_project(
            application_store,
            project_id,
            self.kms.as_ref(),
            |key| std::env::var(key).ok(),
        )
        .await
        .map_err(WorkflowStepError::retryable)?;
        Ok(Arc::new(forge))
    }
}

/// The branch a release's version update lives on, one per release so a
/// re-run finds its own pull request.
#[must_use]
pub fn update_branch(version: &str) -> String {
    format!("navigator/update-{version}")
}

/// Create or adopt one open Project's repository, then read its pin; on
/// Forgejo also govern it and propose the version update. Drive is not part
/// of this sync, so none is passed. Every step is idempotent, so a retry
/// repeats nothing. A failure is returned to the parent, which counts it
/// against this Project and tag.
pub async fn reconcile_repository(
    application_store: &ApplicationStore,
    forge: &RepositoryForge,
    project_id: Uuid,
    runtime_version: &str,
) -> Result<RepositoryOutcome, WorkflowStepError> {
    if matches!(forge, RepositoryForge::None) {
        return Ok(RepositoryOutcome::default());
    }
    let Some(project) = store::projects::find_by_id(application_store, project_id)
        .await
        .map_err(WorkflowStepError::retryable)?
        .filter(|project| project.status == "open")
    else {
        return Ok(RepositoryOutcome::default());
    };
    let selected = forge.for_repository(project.repository_url.as_deref());
    let governed = match &selected {
        RepositoryForge::Forgejo(firms) => {
            Some(firms.for_project(application_store, project_id).await?)
        }
        _ => None,
    };
    let forge: &dyn ForgeService = match (&selected, governed.as_deref()) {
        (_, Some(governed)) => governed,
        (RepositoryForge::GitHub(forge), None) => forge.as_ref(),
        _ => return Ok(RepositoryOutcome::default()),
    };
    if let Some(recorded) = project.repository_url.as_deref() {
        // The origin selects a client. Verify the full URL before using that
        // client's namespace for reads or governance changes.
        let repository = forge.find_repository(&project.code).await.map_err(|_| {
            WorkflowStepError::retryable(anyhow::anyhow!("Recorded repository lookup failed"))
        })?;
        if !repository.is_some_and(|repo| matches_recorded_repository(&repo.url, recorded)) {
            return Err(WorkflowStepError::terminal(
                "Configured forge did not verify the recorded repository",
            ));
        }
    }
    let surfaces = store::project_surfaces::reconcile(
        application_store,
        project_id,
        None::<&cloud::GoogleDrive>,
        Some(forge),
    )
    .await
    .map_err(WorkflowStepError::retryable)?;
    // An empty repository has no `main` to protect and nothing to update.
    let governed = match governed.as_deref() {
        Some(governed)
            if forge
                .head_commit_sha(&project.code)
                .await
                .map_err(WorkflowStepError::retryable)?
                .is_some() =>
        {
            Some(governed)
        }
        _ => None,
    };
    let governance_repaired = match governed {
        Some(governed) => governed
            .reconcile_governance(&project.code)
            .await
            .map_err(WorkflowStepError::retryable)?
            .len(),
        None => 0,
    };
    let (pin, pinned_version, manifest) = read_pin(forge, &project.code, runtime_version).await;
    let update = match (governed, pin, manifest) {
        (Some(governed), PinStatus::Stale, Some(manifest)) => {
            Some(propose_update(governed, &project.code, &manifest, runtime_version).await?)
        }
        _ => None,
    };
    tracing::info!(
        %project_id,
        pin = pin.as_str(),
        pinned_version = pinned_version.as_deref().unwrap_or(""),
        runtime_version,
        governance_repaired,
        update = ?update,
        "Project repository pin checked"
    );
    Ok(RepositoryOutcome {
        synced: usize::from(surfaces.repository_status == SurfaceStatus::Created),
        pin: Some(pin),
        pinned_version,
        runtime_version: Some(runtime_version.to_string()),
        governance_repaired,
        update,
    })
}

/// Rewrite only `version:` and propose it on the release's own branch.
async fn propose_update(
    forge: &dyn GovernedForge,
    project_code: &str,
    manifest: &str,
    runtime_version: &str,
) -> Result<UpdateProposal, WorkflowStepError> {
    let content = with_pinned_version(manifest, runtime_version)
        .map_err(|reason| WorkflowStepError::retryable(anyhow::anyhow!(reason)))?;
    let branch = update_branch(runtime_version);
    let title = format!("Update Navigator to {runtime_version}");
    let body = format!(
        "The Nightly Project Update pins `{MANIFEST}` to Navigator {runtime_version}. \
         Only `version:` changes. Squash auto-merge waits for `navigator project gate`."
    );
    forge
        .propose_file_update(
            project_code,
            &FileUpdate {
                branch: &branch,
                path: MANIFEST,
                content: &content,
                message: &title,
                title: &title,
                body: &body,
            },
        )
        .await
        .map_err(WorkflowStepError::retryable)
}

/// Read the repository's `navigator.yaml` `version:` and grade it, keeping
/// the text for a version update. A forge error, a missing file, or a
/// manifest that does not parse is `unreadable`; none of them fails the
/// sweep.
async fn read_pin(
    forge: &dyn ForgeService,
    project_code: &str,
    runtime_version: &str,
) -> (PinStatus, Option<String>, Option<String>) {
    let contents = match forge.read_file(project_code, MANIFEST).await {
        Ok(contents) => contents,
        Err(error) => {
            tracing::warn!(%error, "Project repository manifest could not be read");
            None
        }
    };
    let pinned = contents
        .as_deref()
        .and_then(|contents| pinned_version(contents).ok().flatten());
    let status = pin_status(pinned.as_deref(), runtime_version);
    (status, pinned, contents)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use cloud::forge::FakeForge;

    /// Every Project resolves to the same in-memory Firm forge.
    struct FakeFirms(FakeForge);

    #[async_trait::async_trait]
    impl FirmForges for FakeFirms {
        async fn for_project(
            &self,
            _application_store: &ApplicationStore,
            _project_id: Uuid,
        ) -> Result<Arc<dyn GovernedForge>, WorkflowStepError> {
            Ok(Arc::new(self.0.clone()))
        }
    }

    pub(crate) fn mixed_forges(github: &FakeForge, forgejo: &FakeForge) -> RepositoryForge {
        RepositoryForge::ByProject {
            default: ForgeBackend::GitHub,
            github: Some(Arc::new(github.clone())),
            forgejo: Arc::new(FakeFirms(forgejo.clone())),
            forgejo_origin: Some(url::Url::parse("https://forge.example/").unwrap().origin()),
        }
    }

    #[test]
    fn a_recorded_origin_selects_only_its_own_provider() {
        let forges = mixed_forges(&FakeForge::new(), &FakeForge::new());
        assert!(matches!(
            forges.for_repository(Some("https://github.com/firm/project")),
            RepositoryForge::GitHub(_)
        ));
        assert!(matches!(
            forges.for_repository(Some("https://forge.example.attacker.test/firm/project")),
            RepositoryForge::None
        ));
    }

    #[test]
    fn the_outcome_journals_versions_and_a_status_only() {
        let json = serde_json::to_string(&RepositoryOutcome {
            synced: 1,
            pin: Some(PinStatus::Stale),
            pinned_version: Some("26.9.13".to_string()),
            runtime_version: Some("26.10.4".to_string()),
            governance_repaired: 2,
            update: Some(UpdateProposal::Opened),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"synced":1,"pin":"stale","pinned_version":"26.9.13","runtime_version":"26.10.4","governance_repaired":2,"update":"opened"}"#
        );
        let journaled: RepositoryOutcome = serde_json::from_str(
            r#"{"synced":0,"pin":"current","pinned_version":"26.10.4","runtime_version":"26.10.4"}"#,
        )
        .expect("an outcome journaled before governance still reads");
        assert_eq!(journaled.update, None);
        assert!(!json.contains("https://"));
        assert!(!json.contains("sample-"));
        assert!(!json.contains("name"));
    }

    #[test]
    fn each_release_has_its_own_update_branch() {
        assert_eq!(update_branch("26.10.5"), "navigator/update-26.10.5");
    }

    #[test]
    fn the_runtime_version_is_the_workspace_release() {
        assert_eq!(
            pin_status(Some(RUNTIME_VERSION), RUNTIME_VERSION),
            PinStatus::Current,
            "{RUNTIME_VERSION} must be a release version so every pin can be graded"
        );
    }
}
