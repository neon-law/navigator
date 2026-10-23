//! The `repository` tag: one open Project's private repository is created or
//! adopted and recorded on the row, then its committed `navigator.yaml` pin
//! is read and compared against the running release. Participation is never
//! copied onto the forge. A closed or archived Project is never given a
//! repository, and a deployment with no forge skips the tag.
//!
//! On Forgejo (`NAVIGATOR_FORGE_BACKEND=forgejo`) the tag works through the
//! Project's own Firm client and, once the repository has a head commit, also
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
use restate_sdk::prelude::*;
use rules::project_manifest::{
    pin_status, pinned_version, with_pinned_version, PinStatus, FILE as MANIFEST,
};
use serde::{Deserialize, Serialize};
use store::project_surfaces::SurfaceStatus;
use store::surreal::SurrealDb;
use uuid::Uuid;

use super::{tag_retry_policy, ProjectTagRequest};

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
}

/// Resolves the Forgejo client confined to one Project's Firm.
#[async_trait::async_trait]
pub trait FirmForges: Send + Sync {
    /// # Errors
    /// When the Firm, its token, or the Forgejo root cannot be resolved.
    async fn for_project(
        &self,
        surreal: &SurrealDb,
        project_id: Uuid,
    ) -> Result<Arc<dyn GovernedForge>, HandlerError>;
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
        surreal: &SurrealDb,
        project_id: Uuid,
    ) -> Result<Arc<dyn GovernedForge>, HandlerError> {
        let forge = store::project_surfaces::forgejo_for_project(
            surreal,
            project_id,
            self.kms.as_ref(),
            |key| std::env::var(key).ok(),
        )
        .await
        .map_err(HandlerError::from)?;
        Ok(Arc::new(forge))
    }
}

#[derive(Clone)]
pub struct RepositoryTagService {
    surreal: SurrealDb,
    forge: RepositoryForge,
    runtime_version: String,
}

impl RepositoryTagService {
    /// `runtime_version` is the release this worker runs, normally
    /// [`RUNTIME_VERSION`].
    #[must_use]
    pub fn new(
        surreal: SurrealDb,
        forge: RepositoryForge,
        runtime_version: impl Into<String>,
    ) -> Self {
        Self {
            surreal,
            forge,
            runtime_version: runtime_version.into(),
        }
    }
}

#[restate_sdk::workflow(name = "NightlyProjectUpdateRepository")]
impl RepositoryTagService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        request: Json<ProjectTagRequest>,
    ) -> Result<Json<RepositoryOutcome>, HandlerError> {
        let project_id = request.into_inner().project_id;
        let surreal = self.surreal.clone();
        let forge = self.forge.clone();
        let runtime_version = self.runtime_version.clone();
        let outcome = ctx
            .run(move || async move {
                reconcile_repository(&surreal, &forge, project_id, &runtime_version)
                    .await
                    .map(Json)
            })
            .name("reconcile-repository")
            .retry_policy(tag_retry_policy())
            .await?;
        Ok(outcome)
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
    surreal: &SurrealDb,
    forge: &RepositoryForge,
    project_id: Uuid,
    runtime_version: &str,
) -> Result<RepositoryOutcome, HandlerError> {
    if matches!(forge, RepositoryForge::None) {
        return Ok(RepositoryOutcome::default());
    }
    let Some(project) = store::projects::find_by_id(surreal, project_id)
        .await
        .map_err(HandlerError::from)?
        .filter(|project| project.status == "open")
    else {
        return Ok(RepositoryOutcome::default());
    };
    let governed = match forge {
        RepositoryForge::Forgejo(firms) => Some(firms.for_project(surreal, project_id).await?),
        RepositoryForge::GitHub(_) | RepositoryForge::None => None,
    };
    let forge: &dyn ForgeService = match (forge, governed.as_deref()) {
        (_, Some(governed)) => governed,
        (RepositoryForge::GitHub(forge), None) => forge.as_ref(),
        _ => return Ok(RepositoryOutcome::default()),
    };
    let surfaces = store::project_surfaces::reconcile(
        surreal,
        project_id,
        None::<&cloud::GoogleDrive>,
        Some(forge),
    )
    .await
    .map_err(HandlerError::from)?;
    // An empty repository has no `main` to protect and nothing to update.
    let governed = match governed.as_deref() {
        Some(governed) if forge.head_commit_sha(&project.code).await?.is_some() => Some(governed),
        _ => None,
    };
    let governance_repaired = match governed {
        Some(governed) => governed.reconcile_governance(&project.code).await?.len(),
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
) -> Result<UpdateProposal, HandlerError> {
    let content = with_pinned_version(manifest, runtime_version)
        .map_err(|reason| HandlerError::from(anyhow::anyhow!(reason)))?;
    let branch = update_branch(runtime_version);
    let title = format!("Update Navigator to {runtime_version}");
    let body = format!(
        "The Nightly Project Update pins `{MANIFEST}` to Navigator {runtime_version}. \
         Only `version:` changes. Squash auto-merge waits for `navigator project gate`."
    );
    Ok(forge
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
        .await?)
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
mod tests {
    use super::*;
    use cloud::forge::FakeForge;
    use store::projects::{create, NewProject};
    use store::test_support::{mem_surreal, seed_entity};

    fn github(forge: &FakeForge) -> RepositoryForge {
        RepositoryForge::GitHub(Arc::new(forge.clone()))
    }

    /// Every Project resolves to the same in-memory Firm forge.
    struct FakeFirms(FakeForge);

    #[async_trait::async_trait]
    impl FirmForges for FakeFirms {
        async fn for_project(
            &self,
            _surreal: &SurrealDb,
            _project_id: Uuid,
        ) -> Result<Arc<dyn GovernedForge>, HandlerError> {
            Ok(Arc::new(self.0.clone()))
        }
    }

    fn forgejo(forge: &FakeForge) -> RepositoryForge {
        RepositoryForge::Forgejo(Arc::new(FakeFirms(forge.clone())))
    }

    const STALE: &str =
        "# Sample manifest\nversion: 26.9.13\nproject:\n  host: staging.example.com\n";

    async fn open_project(surreal: &SurrealDb, code: &str) -> store::projects::Project {
        create(
            surreal,
            &NewProject {
                code: code.to_string(),
                name: format!("Sample {code}"),
                status: "open".to_string(),
                entity_id: seed_entity(surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create Project")
    }

    #[tokio::test]
    async fn a_pin_equal_to_the_runtime_is_current_and_opens_no_pull_request() {
        let surreal = mem_surreal().await;
        let project = open_project(&surreal, "sample-current").await;
        let forge = FakeForge::new();
        forge.set_file(
            &project.code,
            "navigator.yaml",
            "version: 26.10.4\nproject:\n  host: staging.example.com\n  name: sample-current\n",
        );

        let outcome = reconcile_repository(&surreal, &github(&forge), project.id, "26.10.4")
            .await
            .expect("reconcile");
        assert_eq!(outcome.pin, Some(PinStatus::Current));
        assert_eq!(outcome.pinned_version.as_deref(), Some("26.10.4"));
        assert_eq!(outcome.runtime_version.as_deref(), Some("26.10.4"));
        assert_eq!(outcome.synced, 1);
        assert_eq!(
            forge.repository_count(),
            1,
            "the repository was created; nothing else touched the forge"
        );
    }

    #[tokio::test]
    async fn a_pin_behind_the_runtime_is_stale_and_still_opens_no_pull_request() {
        let surreal = mem_surreal().await;
        let project = open_project(&surreal, "sample-stale").await;
        let forge = FakeForge::new();
        forge.set_file(&project.code, "navigator.yaml", "version: 26.9.13\n");

        let outcome = reconcile_repository(&surreal, &github(&forge), project.id, "26.10.4")
            .await
            .expect("reconcile");
        assert_eq!(outcome.pin, Some(PinStatus::Stale));
        assert_eq!(outcome.pinned_version.as_deref(), Some("26.9.13"));
        assert_eq!(
            forge
                .read_file(&project.code, "navigator.yaml")
                .await
                .unwrap()
                .as_deref(),
            Some("version: 26.9.13\n"),
            "the committed manifest is untouched: detection only"
        );
        assert_eq!(forge.ensure_calls(), 1);
        assert_eq!(outcome.update, None);
        assert_eq!(
            forge.governance_reconciles(),
            0,
            "GitHub is not governed here"
        );
        assert!(forge.update_pulls(&project.code).is_empty());
    }

    #[tokio::test]
    async fn a_stale_forgejo_repository_gets_exactly_one_update_pull_request_per_release() {
        let surreal = mem_surreal().await;
        let project = open_project(&surreal, "sample-forgejo-stale").await;
        let forge = FakeForge::new();
        forge.set_file(&project.code, "navigator.yaml", STALE);
        forge.set_head_commit_sha(&project.code, "synthetic-head");

        let first = reconcile_repository(&surreal, &forgejo(&forge), project.id, "26.10.5")
            .await
            .expect("reconcile");
        assert_eq!(first.pin, Some(PinStatus::Stale));
        assert_eq!(first.update, Some(UpdateProposal::Opened));
        let again = reconcile_repository(&surreal, &forgejo(&forge), project.id, "26.10.5")
            .await
            .expect("re-run");
        assert_eq!(again.update, Some(UpdateProposal::Pending));

        let pulls = forge.update_pulls(&project.code);
        assert_eq!(pulls.len(), 1, "one pull request for the release");
        let pull = &pulls["navigator/update-26.10.5"];
        assert_eq!(pull.path, "navigator.yaml");
        assert_eq!(
            pull.content,
            "# Sample manifest\nversion: 26.10.5\nproject:\n  host: staging.example.com\n",
            "only `version:` changes"
        );
        assert_eq!(pull.title, "Update Navigator to 26.10.5");
        assert_eq!(
            forge
                .read_file(&project.code, "navigator.yaml")
                .await
                .unwrap()
                .as_deref(),
            Some(STALE),
            "main is untouched until the pull request merges"
        );

        let next = reconcile_repository(&surreal, &forgejo(&forge), project.id, "26.10.6")
            .await
            .unwrap();
        assert_eq!(
            next.update,
            Some(UpdateProposal::Opened),
            "a new release, a new PR"
        );
        assert_eq!(forge.update_pulls(&project.code).len(), 2);
    }

    #[tokio::test]
    async fn a_forgejo_repository_with_governance_drift_is_repaired_every_night() {
        let surreal = mem_surreal().await;
        let project = open_project(&surreal, "sample-forgejo-drift").await;
        let forge = FakeForge::new();
        forge.set_file(&project.code, "navigator.yaml", "version: 26.10.5\n");
        forge.set_head_commit_sha(&project.code, "synthetic-head");
        forge.set_governance_drift(
            &project.code,
            &["main.enable_push", "repository.allow_merge_commits"],
        );

        let outcome = reconcile_repository(&surreal, &forgejo(&forge), project.id, "26.10.5")
            .await
            .unwrap();
        assert_eq!(outcome.governance_repaired, 2);
        assert_eq!(outcome.pin, Some(PinStatus::Current));
        assert_eq!(outcome.update, None, "a current pin proposes nothing");
        let rerun = reconcile_repository(&surreal, &forgejo(&forge), project.id, "26.10.5")
            .await
            .unwrap();
        assert_eq!(rerun.governance_repaired, 0);
        assert_eq!(forge.governance_reconciles(), 2, "reconciled every run");
    }

    #[tokio::test]
    async fn an_empty_forgejo_repository_is_neither_governed_nor_updated() {
        let surreal = mem_surreal().await;
        let project = open_project(&surreal, "sample-forgejo-empty").await;
        let forge = FakeForge::new();

        let outcome = reconcile_repository(&surreal, &forgejo(&forge), project.id, "26.10.5")
            .await
            .unwrap();
        assert_eq!(outcome.synced, 1, "the repository is still created");
        assert_eq!(outcome.pin, Some(PinStatus::Unreadable));
        assert_eq!(forge.governance_reconciles(), 0);
        assert!(forge.update_pulls(&project.code).is_empty());
    }

    #[tokio::test]
    async fn a_closed_update_pull_request_is_not_reopened() {
        let surreal = mem_surreal().await;
        let project = open_project(&surreal, "sample-forgejo-declined").await;
        let forge = FakeForge::new();
        forge.set_file(&project.code, "navigator.yaml", STALE);
        forge.set_head_commit_sha(&project.code, "synthetic-head");
        reconcile_repository(&surreal, &forgejo(&forge), project.id, "26.10.5")
            .await
            .unwrap();
        forge.close_update_pull(&project.code, "navigator/update-26.10.5");

        let outcome = reconcile_repository(&surreal, &forgejo(&forge), project.id, "26.10.5")
            .await
            .unwrap();
        assert_eq!(outcome.update, Some(UpdateProposal::Declined));
        assert_eq!(forge.update_pulls(&project.code).len(), 1);
    }

    #[tokio::test]
    async fn a_missing_or_unparsable_manifest_is_unreadable_and_does_not_fail_the_sweep() {
        let surreal = mem_surreal().await;
        let missing = open_project(&surreal, "sample-missing").await;
        let broken = open_project(&surreal, "sample-broken").await;
        let rolling = open_project(&surreal, "sample-rolling").await;
        let forge = FakeForge::new();
        forge.set_file(&broken.code, "navigator.yaml", "version: [\n");
        forge.set_file(&rolling.code, "navigator.yaml", "version: latest\n");

        for project in [&missing, &broken, &rolling] {
            let outcome = reconcile_repository(&surreal, &github(&forge), project.id, "26.10.4")
                .await
                .expect("an unreadable manifest is counted, not fatal");
            assert_eq!(outcome.pin, Some(PinStatus::Unreadable), "{}", project.code);
            assert_eq!(outcome.synced, 1);
        }
        assert_eq!(
            reconcile_repository(&surreal, &github(&forge), rolling.id, "26.10.4")
                .await
                .unwrap()
                .pinned_version
                .as_deref(),
            Some("latest"),
            "the text read is reported even when it is not a release version"
        );
    }

    #[tokio::test]
    async fn a_closed_project_and_a_deployment_without_a_forge_report_no_pin() {
        let surreal = mem_surreal().await;
        let closed = create(
            &surreal,
            &NewProject {
                code: "sample-closed".to_string(),
                name: "Sample Closed".to_string(),
                status: "closed".to_string(),
                entity_id: seed_entity(&surreal).await,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let forge = FakeForge::new();
        let outcome = reconcile_repository(&surreal, &github(&forge), closed.id, "26.10.4")
            .await
            .unwrap();
        assert_eq!(outcome, RepositoryOutcome::default());
        let open = open_project(&surreal, "sample-no-forge").await;
        let outcome = reconcile_repository(&surreal, &RepositoryForge::None, open.id, "26.10.4")
            .await
            .unwrap();
        assert_eq!(outcome, RepositoryOutcome::default());
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
