//! Provision the three handles a Project opens with.
//!
//! Opening a Project records its identity. This module then creates or adopts
//! the three external surfaces that identity names:
//!
//! 1. **Working files** — the documents-bucket prefix
//!    `projects/<code>/documents`. The prefix is a key convention, not a
//!    bucket; nothing here writes an object.
//! 2. **Ingest** — the Drive folder named for the code, recorded as
//!    `drive_folder_id`. Drive is import-only: membership lets people drop
//!    files in; Navigator never treats the folder as a live store.
//! 3. **Source** — one private repository named for the code, recorded as
//!    `repository_url`. Project participation is never copied onto the forge.
//!
//! Each step is idempotent. A folder or repository that already exists is
//! adopted. A column that is already set is left alone, so a retry after a
//! partial external failure cannot duplicate a handle. Absent Drive or forge
//! configuration skips that surface rather than failing the matter open.

use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

use cloud::drive::{DriveMember, DriveMemberKind, DriveRole, DriveService};
use cloud::forge::{ForgeError, ForgeService};
use cloud::workspace::documents_prefix;
use cloud::DriveError;

use crate::application_store::ApplicationStore;
use crate::persons;
use crate::projects::{self, set_drive_folder_id, ProjectCommandError, ProjectStoreError};
pub use model::project_surfaces::{
    DriveFolderCheck, ProjectHandles, ProjectSurfaces, SurfaceStatus, DRIVE_FOLDER_CHECK_HEADER,
};

/// The Projects shared drive alone declares Drive: the delegation keys are
/// shared with Calendar and Contacts, so a deployment can carry them with no
/// Drive at all.
const DRIVE_CONFIGURATION_KEYS: [&str; 1] = ["NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID"];

/// The deployment's public host, which a seeded Project manifest pins as
/// the gate workflow's OIDC audience.
const NAVIGATOR_PUBLIC_HOST_ENV: &str = "NAVIGATOR_PUBLIC_HOST";

/// The release tag a seeded Project manifest pins; defaults to this
/// workspace's version when the deployment does not name one.
const NAVIGATOR_RELEASE_TAG_ENV: &str = "NAVIGATOR_RELEASE_TAG";

const FORGE_CONFIGURATION_KEYS: [&str; 8] = [
    cloud::workspace::NAVIGATOR_GITHUB_ORG,
    cloud::workspace::NAVIGATOR_GIT_HOST,
    cloud::forge::NAVIGATOR_GITHUB_TOKEN_ENV,
    cloud::forge::GITHUB_TOKEN_ENV,
    cloud::forge::GITHUB_API_BASE_ENV,
    cloud::forge::NAVIGATOR_GITHUB_APP_ID_ENV,
    cloud::forge::NAVIGATOR_GITHUB_INSTALLATION_ID_ENV,
    cloud::forge::NAVIGATOR_GITHUB_APP_PRIVATE_KEY_ENV,
];

/// A provider is disabled only when every key that opts it in is absent.
/// A present but blank value is configuration too, so the provider constructor
/// can report the exact missing coordinate instead of quietly skipping it.
fn provider_configured<F>(keys: &[&str], get: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    keys.iter().any(|key| get(key).is_some())
}

/// The reconcile detail for a Drive folder the Project already records.
pub const DRIVE_FOLDER_ALREADY_SET: &str =
    "adopted the folder already set on the Project; no folder created";

/// Confirm a Drive folder id before it is recorded on a Project: with a
/// provider, it must be a live folder in the Firm's Projects shared drive
/// ([`DriveService::is_ingest_folder`], the same test an import makes); with
/// none, the check is [`DriveFolderCheck::Skipped`].
///
/// # Errors
/// [`SurfaceError::Command`] carrying
/// [`projects::DRIVE_FOLDER_NOT_IN_PROJECTS_DRIVE`] when the provider does not
/// find the folder, and [`SurfaceError::Drive`] when it cannot be asked.
pub async fn check_drive_folder<D>(
    drive: Option<&D>,
    folder_id: &str,
) -> Result<DriveFolderCheck, SurfaceError>
where
    D: DriveService + ?Sized,
{
    let Some(drive) = drive else {
        return Ok(DriveFolderCheck::Skipped);
    };
    if drive.is_ingest_folder(folder_id).await? {
        Ok(DriveFolderCheck::Verified)
    } else {
        Err(SurfaceError::Command(ProjectCommandError::Invalid(
            projects::DRIVE_FOLDER_NOT_IN_PROJECTS_DRIVE,
        )))
    }
}

/// [`check_drive_folder`] against the provider the process environment
/// configures, on the same terms as [`reconcile_from_env`]: no Drive key at
/// all skips the check, and a partial configuration is an error.
///
/// # Errors
/// As [`check_drive_folder`], plus a provider initialization failure.
pub async fn check_drive_folder_from_env(
    folder_id: &str,
) -> Result<DriveFolderCheck, SurfaceError> {
    if !provider_configured(&DRIVE_CONFIGURATION_KEYS, |key| std::env::var(key).ok()) {
        return Ok(DriveFolderCheck::Skipped);
    }
    let drive = cloud::GoogleDrive::from_env().await?;
    check_drive_folder(Some(&drive), folder_id).await
}

/// A Project's repository-provisioning state, read from
/// [`crate::projects::Project::repository_url`],
/// [`crate::projects::Project::forge_provisioned_at`], and
/// [`crate::projects::Project::git_initialized_at`] together rather than one
/// column at a time — a caller reading only `repository_url` cannot tell
/// "never requested" from "requested but the forge failed" from "attached
/// but no source has landed yet."
///
/// Pure derivation ([`source_state`]): no database write, no forge or Drive
/// call, and no timestamp is invented for a state the columns do not already
/// record. A row with every column null reports [`SourceState::NotEnabled`],
/// never [`SourceState::Pending`] or [`SourceState::Failed`] — an absent
/// repository is a legitimate, common resting state, not a stalled or broken
/// one (see `docs/project-repositories.md#an-absent-repository-is-legitimate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// No repository is recorded and reconcile has never stamped a failed
    /// attempt either. The default resting state for a matter that has not
    /// asked for one — [`crate::projects::Project::repository_url`] is
    /// nullable exactly so this can be true forever.
    NotEnabled,
    /// A repository has been requested but reconcile has not yet resolved
    /// it. No writer in this codebase leaves a row in this state today:
    /// [`reconcile`] either records a repository in the same pass that asks
    /// for one, or does not attempt one at all. Reserved for a future
    /// asynchronous provisioning path.
    Pending,
    /// A repository URL is recorded, but not stamped by this deployment's
    /// own provisioning pass — `forge_provisioned_at` is unset, exactly the
    /// shape a direct `PATCH` of `repository_url` or a row written before
    /// that column existed produces. The columns cannot say which, so this
    /// is reported for human reconciliation rather than guessed.
    Unknown,
    /// Recorded and stamped provisioned; no validated source has been
    /// committed or imported yet.
    Attached,
    /// Recorded, provisioned, and carrying validated source.
    Initialized,
    /// Reconcile attempted a repository and the attempt did not succeed.
    /// Today a failed attempt leaves every column exactly as it was —
    /// indistinguishable from never having tried — so no writer produces
    /// this state yet. Reserved for a future persisted failure marker.
    Failed,
}

impl SourceState {
    /// The wire spelling — the same string [`serde::Serialize`] produces,
    /// exposed as a plain method so a CLI table or log line can print it
    /// without round-tripping through JSON.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SourceState::NotEnabled => "not_enabled",
            SourceState::Pending => "pending",
            SourceState::Unknown => "unknown",
            SourceState::Attached => "attached",
            SourceState::Initialized => "initialized",
            SourceState::Failed => "failed",
        }
    }
}

/// Derive [`SourceState`] from one Project's own columns. See the type's own
/// documentation for why each state is or is not reachable today.
#[must_use]
pub fn source_state(project: &crate::projects::Project) -> SourceState {
    match (
        project.repository_url.is_some(),
        project.forge_provisioned_at.is_some(),
        project.git_initialized_at.is_some(),
    ) {
        (false, _, _) => SourceState::NotEnabled,
        (true, false, _) => SourceState::Unknown,
        (true, true, false) => SourceState::Attached,
        (true, true, true) => SourceState::Initialized,
    }
}

#[derive(Debug, Error)]
pub enum SurfaceError {
    #[error("no matter with that id")]
    NotFound,
    #[error("Drive folder reconciliation blocked: {0}")]
    FolderBlocked(&'static str),
    #[error("drive: {0}")]
    Drive(#[from] DriveError),
    #[error("forge: {0}")]
    Forge(#[from] ForgeError),
    #[error("{0}")]
    Command(#[from] ProjectCommandError),
    #[error("database: {0}")]
    Store(#[from] ProjectStoreError),
    #[error("database: {0}")]
    Db(String),
    #[error("Forgejo provisioning needs {0}")]
    ForgejoUnavailable(String),
    #[error("Forgejo could not load the owning Firm's credential: {0}")]
    ForgejoCredential(#[source] crate::firm_secrets::SecretStoreError),
}

/// Read one Project's manifest coordinates without reconciling or writing any
/// external resource. The Project row remains authoritative for links; DRI
/// emails are derived from its participation markers and Person rows.
pub async fn read_handles(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<ProjectHandles, SurfaceError> {
    let project = projects::find_by_id(application_store, project_id)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?
        .ok_or(SurfaceError::NotFound)?;
    let participations = projects::participations_for_project(application_store, project_id)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?;
    let dri_ids: Vec<_> = participations
        .iter()
        .filter(|row| row.is_lawyer_dri || row.is_client_dri)
        .map(|row| row.person_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let people = crate::persons::find_by_ids(application_store, &dri_ids)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?;
    let found_ids: std::collections::HashSet<_> = people.iter().map(|person| person.id).collect();
    let mut dangling_dri_person_ids: Vec<Uuid> = dri_ids
        .into_iter()
        .filter(|id| !found_ids.contains(id))
        .collect();
    dangling_dri_person_ids.sort_unstable();
    if !dangling_dri_person_ids.is_empty() {
        tracing::warn!(
            project_id = %project_id,
            dangling_dri_person_ids = ?dangling_dri_person_ids,
            "Project DRI participation refers to a missing Person; skipping it"
        );
    }
    let emails: std::collections::HashMap<_, _> = people
        .into_iter()
        .map(|person| (person.id, person.email))
        .collect();
    let mut lawyer_dri = Vec::new();
    let mut client_dri = Vec::new();
    for row in participations {
        if row.is_lawyer_dri {
            if let Some(email) = emails.get(&row.person_id) {
                lawyer_dri.push(email.clone());
            }
        }
        if row.is_client_dri {
            if let Some(email) = emails.get(&row.person_id) {
                client_dri.push(email.clone());
            }
        }
    }
    lawyer_dri.sort_unstable();
    lawyer_dri.dedup();
    client_dri.sort_unstable();
    client_dri.dedup();
    let legal_plan = manifest_legal_plan(&project).await?;
    Ok(ProjectHandles {
        code: project.code,
        repository_url: project.repository_url,
        lawyer_dri,
        client_dri,
        private_slack_channel: project.internal_slack_channel_url,
        private_notion_page: project.private_notion_page_url,
        calendar_id: project.calendar_id,
        drive_folder_id: project.drive_folder_id,
        legal_plan,
        dangling_dri_person_ids,
    })
}

/// Create or adopt the Project's Drive ingest folder and source repository,
/// and name its documents-bucket prefix.
///
/// `drive` and `forge` are independently optional. A KIND loop with neither
/// still opens matters; an admin reconcile later fills in whichever service
/// the deployment has configured.
pub async fn reconcile<D, F>(
    application_store: &ApplicationStore,
    project_id: Uuid,
    drive: Option<&D>,
    forge: Option<&F>,
) -> Result<ProjectSurfaces, SurfaceError>
where
    D: DriveService + ?Sized,
    F: ForgeService + ?Sized,
{
    let project = projects::find_by_id(application_store, project_id)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?
        .ok_or(SurfaceError::NotFound)?;

    let (drive_folder_id, drive_status, drive_detail) =
        if let Some(existing) = project.drive_folder_id.as_deref() {
            // A folder recorded by hand (`navigator project set
            // --drive-folder` or the matter edit form) is adopted as it
            // stands; setup never creates a second one beside it.
            (
                Some(existing.to_string()),
                SurfaceStatus::Present,
                Some(DRIVE_FOLDER_ALREADY_SET.to_string()),
            )
        } else if let Some(drive) = drive {
            // LAW-213: a folder named for the code that predates the matter
            // is adopted, not shadowed by a second folder. Only a genuinely
            // absent folder is created.
            let (folder_id, status) =
                if let Some(existing) = drive.find_folder_by_name(&project.code).await? {
                    set_drive_folder_id(application_store, project_id, Some(&existing.id))
                        .await?
                        .ok_or(SurfaceError::NotFound)?;
                    (existing.id, SurfaceStatus::Adopted)
                } else {
                    let folder = drive.create_folder(&project.code).await?;
                    set_drive_folder_id(application_store, project_id, Some(&folder.id))
                        .await?
                        .ok_or(SurfaceError::NotFound)?;
                    (folder.id, SurfaceStatus::Created)
                };
            (Some(folder_id), status, None)
        } else {
            (
                None,
                SurfaceStatus::Skipped,
                Some("Drive provider is not configured".to_string()),
            )
        };

    if let (Some(drive), Some(folder_id)) = (drive, drive_folder_id.as_deref()) {
        grant_drive_ingest_membership(application_store, project_id, drive, folder_id).await?;
    }

    let (repository_url, repository_status, repository_detail) =
        if let Some(existing) = project.repository_url.as_deref() {
            (Some(existing.to_string()), SurfaceStatus::Present, None)
        } else if let Some(forge) = forge {
            let repository = forge.ensure_repository(&project.code).await?;
            record_provisioned_repository(application_store, project_id, &repository.url).await?;
            (Some(repository.url), SurfaceStatus::Created, None)
        } else {
            (
                None,
                SurfaceStatus::Skipped,
                Some("repository provider is not configured".to_string()),
            )
        };

    Ok(ProjectSurfaces {
        code: project.code.clone(),
        documents_prefix: documents_prefix(&project.code),
        drive_folder_id,
        drive_status,
        drive_detail,
        repository_url,
        repository_status,
        repository_detail,
    })
}

/// Resolve Drive and forge from the process environment and reconcile.
///
/// An entirely absent provider configuration disables that integration.
/// Once any opt-in key is present, initialization errors are returned so a
/// partial or invalid deployment configuration cannot masquerade as skipped.
/// With `NAVIGATOR_FORGE_BACKEND=forgejo`, the repository is created in the
/// Project's Firm namespace with that Firm's own encrypted token.
pub async fn reconcile_from_env(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<ProjectSurfaces, SurfaceError> {
    let drive = if provider_configured(&DRIVE_CONFIGURATION_KEYS, |key| std::env::var(key).ok()) {
        Some(cloud::GoogleDrive::from_env().await?)
    } else {
        None
    };
    if drive.is_some() {
        let report = Box::pin(crate::project_folder_reconcile::reconcile_from_env(
            application_store,
            project_id,
        ))
        .await
        .map_err(|_| SurfaceError::Db("Drive folder reconciliation failed".to_string()))?;
        if let Some(report) = report {
            if let crate::project_folder_reconcile::FolderOutcome::Blocked(block) = report.outcome {
                return Err(SurfaceError::FolderBlocked(block.reason()));
            }
        }
    }
    let lookup = |key: &str| std::env::var(key).ok();
    match cloud::ForgeBackend::from_lookup(lookup)? {
        cloud::ForgeBackend::GitHub => {
            let forge = forge_from_env()?;
            reconcile(
                application_store,
                project_id,
                drive.as_ref(),
                forge.as_ref(),
            )
            .await
        }
        cloud::ForgeBackend::Forgejo => {
            let kms = cloud::runtime_kms_from_env().await;
            let forge =
                forgejo_for_project(application_store, project_id, kms.as_ref(), lookup).await?;
            let root =
                lookup(cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV).unwrap_or_default();
            let seed = match scaffold_inputs(application_store, project_id).await {
                Ok(seed) => seed,
                Err(error) => {
                    tracing::error!(
                        %error,
                        project_id = %project_id,
                        "could not determine the scaffold coordinates; the Project repository was not seeded"
                    );
                    ScaffoldInputs::unavailable()
                }
            };
            reconcile_forgejo(
                application_store,
                project_id,
                drive.as_ref(),
                &forge,
                ForgeSite {
                    root: &root,
                    owner: forge.owner(),
                },
                &seed,
            )
            .await
        }
    }
}

/// Where the Firm's Forgejo lives and whom it names: the coordinates a
/// recorded repository URL is checked against before the pass governs or
/// seeds it.
struct ForgeSite<'a> {
    root: &'a str,
    owner: &'a str,
}

/// Whether `url` names `owner`'s namespace on the Forgejo at `root` — a
/// repository this deployment provisioned and may govern. A recorded URL
/// from another forge (a matter whose source deliberately lives elsewhere)
/// fails the prefix and is left entirely alone.
#[must_use]
fn is_firm_forges_repository(url: &str, site: &ForgeSite<'_>) -> bool {
    let root = site.root.trim().trim_end_matches('/');
    url.starts_with(&format!("{root}/{}/", site.owner))
}

/// The Forgejo provisioning pass: reconcile the surfaces, then converge the
/// resolved repository onto Firm governance, and seed an empty repository
/// with the canonical scaffold when the deployment's coordinates are known.
///
/// Convergence reaches a repository only when this pass created or adopted
/// it, or when its recorded URL names the Firm's own namespace on this
/// deployment's Forgejo. Both halves are idempotent — governance re-locks
/// drifted settings, and a seed whose `Source-Tree` digest already sits at
/// the head writes nothing — so the pass runs on every reconcile and a
/// repository that lost either half recovers without a human.
///
/// A convergence fault is loud but non-rollback, mirroring
/// [`reconcile_after_open`]: the provisioned surfaces stand, the fault is
/// logged, and the next pass — a retry through the admin door or the
/// nightly ops reconcile — repairs it.
async fn reconcile_forgejo<D, F>(
    application_store: &ApplicationStore,
    project_id: Uuid,
    drive: Option<&D>,
    forge: &F,
    site: ForgeSite<'_>,
    seed: &ScaffoldInputs,
) -> Result<ProjectSurfaces, SurfaceError>
where
    D: DriveService + ?Sized,
    F: cloud::forgejo_governance::GovernedForge + ?Sized,
{
    let surfaces = reconcile(application_store, project_id, drive, Some(forge)).await?;
    let Some(url) = surfaces.repository_url.as_deref() else {
        return Ok(surfaces);
    };
    if surfaces.repository_status != SurfaceStatus::Created
        && !is_firm_forges_repository(url, &site)
    {
        return Ok(surfaces);
    }
    let code = surfaces.code.clone();

    // Seed before governing: the scaffold's first commit is a direct
    // contents write to `main`, which the protection this pass applies
    // next would refuse.
    if let Err(error) = seed_provisioned_repository(forge, &code, seed).await {
        if matches!(error, ForgeError::RepositoryNotEmpty { .. }) {
            tracing::info!(
                project_id = %project_id,
                "Project repository already holds other source; the canonical scaffold was not seeded"
            );
        } else {
            tracing::error!(
                %error,
                project_id = %project_id,
                "could not seed the Project repository with the canonical scaffold; retry with `navigator projects surfaces reconcile`"
            );
        }
    }

    match forge.reconcile_governance(&code).await {
        Ok(repairs) if repairs.is_empty() => {}
        Ok(repairs) => tracing::info!(
            project_id = %project_id,
            repairs = ?repairs,
            "repaired Forgejo governance controls on the Project repository"
        ),
        Err(error) => tracing::error!(
            %error,
            project_id = %project_id,
            "could not govern the Project repository after provisioning; retry with `navigator projects surfaces reconcile`"
        ),
    }
    Ok(surfaces)
}

/// The coordinates a seeded manifest pins: the deployment's public host —
/// the gate workflow's OIDC audience — the release version, and the first
/// designated lawyer and client DRI emails (the lexicographically first
/// when a Project designates several on a side).
struct ScaffoldInputs {
    host: Option<String>,
    version: String,
    lawyer_dri: String,
    client_dri: String,
    legal_plan: Option<String>,
}

impl ScaffoldInputs {
    /// Coordinates for a pass that could not read them: nothing is seeded,
    /// and governance still runs.
    fn unavailable() -> Self {
        Self {
            host: None,
            version: env!("CARGO_PKG_VERSION").to_string(),
            lawyer_dri: String::new(),
            client_dri: String::new(),
            legal_plan: None,
        }
    }
}

/// Resolve the scaffold coordinates from the deployment environment and the
/// Project's participation.
async fn scaffold_inputs(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<ScaffoldInputs, SurfaceError> {
    let (lawyer_dri, client_dri) = dri_emails(application_store, project_id).await?;
    let project = crate::projects::find_by_id(application_store, project_id)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?
        .ok_or(SurfaceError::NotFound)?;
    let legal_plan = manifest_legal_plan(&project).await?;
    Ok(ScaffoldInputs {
        host: std::env::var(NAVIGATOR_PUBLIC_HOST_ENV)
            .ok()
            .filter(|host| !host.trim().is_empty()),
        version: std::env::var(NAVIGATOR_RELEASE_TAG_ENV)
            .ok()
            .filter(|version| !version.trim().is_empty())
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
        lawyer_dri,
        client_dri,
        legal_plan,
    })
}

/// Resolve the manifest's plan name without copying claim or member information.
async fn manifest_legal_plan(project: &projects::Project) -> Result<Option<String>, SurfaceError> {
    let Some(plan) = project.legal_plan_id else {
        return Ok(None);
    };
    let firm = project
        .firm_id
        .ok_or_else(|| SurfaceError::Db("a Project with a Legal Plan needs a Firm".into()))?;
    let pg = crate::postgres::PostgresDb::application()
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?;
    let plan = crate::legal_plans::find(&pg, firm, plan)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?
        .ok_or_else(|| SurfaceError::Db("the linked Legal Plan is unavailable".into()))?;
    Ok(Some(plan.name))
}

/// The first designated lawyer and client DRI emails for a Project — the
/// same derivation [`read_handles`] reports in full, narrowed to the one
/// email per side the scaffold manifest carries.
async fn dri_emails(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<(String, String), SurfaceError> {
    let participations = projects::participations_for_project(application_store, project_id)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?;
    let dri_ids: Vec<_> = participations
        .iter()
        .filter(|row| row.is_lawyer_dri || row.is_client_dri)
        .map(|row| row.person_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let people = crate::persons::find_by_ids(application_store, &dri_ids)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?;
    let emails: std::collections::HashMap<_, _> = people
        .into_iter()
        .map(|person| (person.id, person.email))
        .collect();
    let mut lawyer_dri = participations
        .iter()
        .filter(|row| row.is_lawyer_dri)
        .filter_map(|row| emails.get(&row.person_id).cloned())
        .collect::<Vec<_>>();
    lawyer_dri.sort_unstable();
    lawyer_dri.dedup();
    let mut client_dri = participations
        .iter()
        .filter(|row| row.is_client_dri)
        .filter_map(|row| emails.get(&row.person_id).cloned())
        .collect::<Vec<_>>();
    client_dri.sort_unstable();
    client_dri.dedup();
    Ok((
        lawyer_dri.first().cloned().unwrap_or_default(),
        client_dri.first().cloned().unwrap_or_default(),
    ))
}

/// Seed an empty Project repository with the canonical scaffold as its
/// first commit. `initialize_source` is digest-idempotent: a retry that
/// finds the same `Source-Tree` digest at the head writes nothing, and a
/// repository holding other source comes back
/// [`ForgeError::RepositoryNotEmpty`] and is preserved, never replaced.
async fn seed_provisioned_repository<F>(
    forge: &F,
    code: &str,
    inputs: &ScaffoldInputs,
) -> Result<(), ForgeError>
where
    F: ForgeService + ?Sized,
{
    let Some(host) = inputs.host.as_deref() else {
        // Without the deployment host the manifest's gate audience would be
        // a lie; leave the repository for a reconcile that knows it.
        return Ok(());
    };
    let files = project_scaffold::forgejo_seed_files(&project_scaffold::ScaffoldSource {
        code,
        host,
        lawyer_dri: &inputs.lawyer_dri,
        client_dri: &inputs.client_dri,
        legal_plan: inputs.legal_plan.as_deref(),
        version: &inputs.version,
    });
    let tree_digest = project_scaffold::source_tree_digest(&files);
    let files: std::collections::BTreeMap<String, String> = files.into_iter().collect();
    forge
        .initialize_source(code, &files, &tree_digest, "Initialize project source")
        .await?;
    Ok(())
}

/// The Forgejo client confined to `project_id`'s Firm namespace and
/// authenticated with that Firm's own access token. Every missing piece is
/// named at once, and nothing falls back to another Firm or an ambient token.
///
/// # Errors
/// [`SurfaceError::ForgejoUnavailable`] naming each missing setting, or a
/// store error.
pub async fn forgejo_for_project(
    application_store: &ApplicationStore,
    project_id: Uuid,
    kms: &dyn cloud::RuntimeKms,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<cloud::ForgejoForge, SurfaceError> {
    let project = projects::find_by_id(application_store, project_id)
        .await?
        .ok_or(SurfaceError::NotFound)?;
    forgejo_for_firm(application_store, project.firm_id, kms, lookup).await
}

/// Resolve Forgejo for an already authorized owning Firm.
/// A caller with a Project snapshot can use its authorized Firm ID directly.
///
/// # Errors
/// Returns a configuration, credential or store error.
pub async fn forgejo_for_firm(
    application_store: &ApplicationStore,
    firm_id: Option<Uuid>,
    kms: &dyn cloud::RuntimeKms,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<cloud::ForgejoForge, SurfaceError> {
    let root = lookup(cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV)
        .filter(|value| !value.trim().is_empty());
    let firm = match firm_id {
        Some(firm_id) => crate::firms::find_by_id(application_store, firm_id)
            .await
            .map_err(|error| SurfaceError::Db(error.to_string()))?,
        None => None,
    };
    let token = match firm.as_ref() {
        Some(firm) => {
            match crate::firm_secrets::resolve_firm_credential(
                application_store,
                firm.id,
                crate::firm_secrets::IntegrationProvider::Forgejo,
                crate::firm_secrets::IntegrationSecretKind::ForgejoAccessToken,
                kms,
            )
            .await
            {
                Ok(token) => Some(token),
                Err(crate::firm_secrets::SecretStoreError::NotConfigured { .. }) => None,
                Err(error) => return Err(SurfaceError::ForgejoCredential(error)),
            }
        }
        None => None,
    };
    let mut missing = Vec::new();
    if root.is_none() {
        missing.push(cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV);
    }
    if firm.is_none() {
        missing.push("an owning Firm on the Project");
    } else if token.is_none() {
        missing.push("the Firm's forgejo_access_token integration secret");
    }
    match (root, firm, token) {
        (Some(root), Some(firm), Some(token)) if missing.is_empty() => {
            let namespace = forgejo_namespace(firm.id, firm.slug).await?;
            Ok(cloud::ForgejoForge::new(
                &root,
                &namespace,
                token.expose_for_provider_call(),
            )?)
        }
        _ => Err(SurfaceError::ForgejoUnavailable(missing.join(", "))),
    }
}

async fn forgejo_namespace(firm: Uuid, slug: String) -> Result<String, SurfaceError> {
    let pg = crate::postgres::PostgresDb::application()
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?;
    let recorded = crate::firms::forgejo_organization::find(&pg, firm)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?;
    Ok(recorded.map_or(slug, |record| record.organization))
}

/// The deployment's GitHub forge, or `None` when every forge opt-in key is
/// absent. A partly configured forge is an error, never a silent skip.
pub fn forge_from_env() -> Result<Option<cloud::forge::GitHubForge>, ForgeError> {
    if provider_configured(&FORGE_CONFIGURATION_KEYS, |key| std::env::var(key).ok()) {
        cloud::forge::GitHubForge::from_env().map(Some)
    } else {
        Ok(None)
    }
}

/// Best-effort wrapper for matter-open doors: a Drive or forge fault is
/// logged rather than rolling the open back. Retry is [`reconcile`].
pub async fn reconcile_after_open(application_store: &ApplicationStore, project_id: Uuid) {
    if let Err(error) = Box::pin(reconcile_from_env(application_store, project_id)).await {
        tracing::error!(
            %error,
            project_id = %project_id,
            "Project surface reconcile failed; retry with `navigator projects surfaces reconcile`"
        );
    }
}

/// Share the ingest folder with every participant who has an email.
///
/// Drive sharing is how Workspace users drop files in. It is not an
/// authorization decision inside Navigator, and it is not a forge grant —
/// GitHub membership APIs are not called from this module.
async fn grant_drive_ingest_membership<D>(
    application_store: &ApplicationStore,
    project_id: Uuid,
    drive: &D,
    folder_id: &str,
) -> Result<(), SurfaceError>
where
    D: DriveService + ?Sized,
{
    let rows = projects::participations_for_project(application_store, project_id).await?;
    for row in rows {
        let Some(person) = persons::find_by_id(application_store, row.person_id)
            .await
            .map_err(|error| SurfaceError::Db(error.to_string()))?
        else {
            continue;
        };
        let viewer = crate::access::matter_viewer(
            application_store,
            Some(person.id),
            person.role,
            project_id,
        )
        .await
        .map_err(SurfaceError::Db)?;
        if !viewer.is_some_and(crate::access::MatterViewer::is_firm_side) {
            continue;
        }
        let email = person.email.trim();
        if email.is_empty() {
            continue;
        }
        drive
            .set_member_permission(
                folder_id,
                &DriveMember {
                    kind: DriveMemberKind::User,
                    email: email.to_string(),
                },
                DriveRole::Writer,
            )
            .await?;
    }
    Ok(())
}

async fn record_provisioned_repository(
    application_store: &ApplicationStore,
    project_id: Uuid,
    url: &str,
) -> Result<(), SurfaceError> {
    projects::record_provisioned_repository(
        application_store,
        project_id,
        url,
        &Utc::now().to_rfc3339(),
    )
    .await?
    .ok_or(SurfaceError::NotFound)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn forgejo_namespace_uses_the_recorded_organization_for_the_selected_firm() {
        crate::test_support::with_native_application_fixture(|_, pg| async move {
            let first = uuid::Uuid::new_v4();
            let second = uuid::Uuid::new_v4();
            sqlx::query("INSERT INTO firm (id, name, slug, status) VALUES ($1, 'Synthetic A', 'synthetic-a', 'active'), ($2, 'Synthetic B', 'synthetic-b', 'active')")
                .bind(first).bind(second).execute(pg.pool()).await.unwrap();
            sqlx::query("INSERT INTO firm_forgejo_organization (firm_id, organization, forgejo_id, owner_login) VALUES ($1, 'verified-a', 1, 'fixture')")
                .bind(first).execute(pg.pool()).await.unwrap();
            assert_eq!(
                super::forgejo_namespace(first, "synthetic-a".into())
                    .await
                    .unwrap(),
                "verified-a"
            );
            assert_eq!(
                super::forgejo_namespace(second, "synthetic-b".into())
                    .await
                    .unwrap(),
                "synthetic-b"
            );
        })
        .await;
    }

    use super::{source_state, SourceState, SurfaceError};

    use crate::projects::{self};

    use cloud::drive::{DriveService, FakeDrive};

    use cloud::workspace::documents_prefix;

    #[test]
    fn partial_provider_configuration_is_not_treated_as_disabled() {
        let drive_keys = super::DRIVE_CONFIGURATION_KEYS;
        let forge_keys = [
            "NAVIGATOR_GITHUB_ORG",
            "NAVIGATOR_GIT_HOST",
            "NAVIGATOR_GITHUB_TOKEN",
            "GITHUB_TOKEN",
            "NAVIGATOR_GITHUB_API_BASE",
            "NAVIGATOR_GITHUB_APP_ID",
            "NAVIGATOR_GITHUB_INSTALLATION_ID",
            "NAVIGATOR_GITHUB_APP_PRIVATE_KEY",
        ];

        assert!(!super::provider_configured(&drive_keys, |_| None));
        assert!(super::provider_configured(&drive_keys, |key| {
            (key == "NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID").then(|| "drive-id".to_string())
        }));
        // Calendar's delegation alone does not declare Drive.
        assert!(!super::provider_configured(&drive_keys, |key| {
            (key == "NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER")
                .then(|| "calendar@example.com".to_string())
        }));
        assert!(!super::provider_configured(&forge_keys, |_| None));
        assert!(super::provider_configured(&forge_keys, |key| {
            (key == "NAVIGATOR_GITHUB_ORG").then(|| "   ".to_string())
        }));
        assert!(super::provider_configured(&forge_keys, |key| {
            (key == "NAVIGATOR_GITHUB_APP_ID").then(|| "12345".to_string())
        }));
    }

    #[test]
    fn the_documents_prefix_is_the_project_code() {
        assert_eq!(documents_prefix("acme"), "projects/acme/documents");
    }

    #[test]
    fn this_module_does_not_read_a_local_drive_mount() {
        let src = include_str!("project_surfaces.rs");
        let production = src
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes the test module");
        assert!(
            !production.contains("DRIVE_MOUNT"),
            "provisioning must not take a workstation mount as input"
        );
        assert!(
            !production.contains("collaborator"),
            "Project participation must not grant forge membership"
        );
    }

    /// The check a hand-set folder passes before it is recorded: a live
    /// folder in the Projects shared drive is verified, an unknown id is
    /// refused with the reason, and no provider skips the check.
    #[tokio::test]
    async fn a_named_drive_folder_is_checked_against_the_provider() {
        let drive = FakeDrive::default();
        let folder = drive.create_folder("acme").await.expect("folder");
        assert_eq!(
            super::check_drive_folder(Some(&drive), &folder.id)
                .await
                .expect("verified"),
            super::DriveFolderCheck::Verified
        );
        let refused = super::check_drive_folder(Some(&drive), "1UnknownSyntheticFolder")
            .await
            .expect_err("an unknown folder is refused");
        assert!(
            matches!(
                refused,
                SurfaceError::Command(projects::ProjectCommandError::Invalid(message))
                    if message == crate::projects::DRIVE_FOLDER_NOT_IN_PROJECTS_DRIVE
            ),
            "{refused:?}"
        );
        assert_eq!(
            super::check_drive_folder(None::<&FakeDrive>, "1UnknownSyntheticFolder")
                .await
                .expect("skipped"),
            super::DriveFolderCheck::Skipped
        );
    }

    #[test]
    fn source_state_reports_no_repository_as_not_enabled() {
        let project = test_project(None, None, None);
        assert_eq!(source_state(&project), SourceState::NotEnabled);
    }

    #[test]
    fn source_state_reports_an_unstamped_url_as_unknown() {
        let project = test_project(Some("https://forge.example/acme"), None, None);
        assert_eq!(source_state(&project), SourceState::Unknown);
    }

    #[test]
    fn source_state_reports_a_stamped_url_with_no_source_as_attached() {
        let project = test_project(
            Some("https://forge.example/acme"),
            Some("2026-09-01T00:00:00Z"),
            None,
        );
        assert_eq!(source_state(&project), SourceState::Attached);
    }

    #[test]
    fn source_state_reports_a_stamped_url_with_source_as_initialized() {
        let project = test_project(
            Some("https://forge.example/acme"),
            Some("2026-09-01T00:00:00Z"),
            Some("2026-09-02T00:00:00Z"),
        );
        assert_eq!(source_state(&project), SourceState::Initialized);
    }

    /// `as_str` and `serde::Serialize` must agree, or a CLI table and the
    /// JSON a caller parses would name the same state two different ways.
    #[test]
    fn as_str_matches_the_serialized_wire_spelling() {
        for state in [
            SourceState::NotEnabled,
            SourceState::Pending,
            SourceState::Unknown,
            SourceState::Attached,
            SourceState::Initialized,
            SourceState::Failed,
        ] {
            let serialized = serde_json::to_value(state).expect("serialize");
            assert_eq!(serialized, state.as_str(), "{state:?}");
        }
    }

    /// A minimal in-memory [`projects::Project`] carrying only the three
    /// columns [`source_state`] reads — the other fields do not vary
    /// across these table-driven cases, so this stays a plain constructor
    /// rather than dragging every test through [`open_acme`] and a real
    /// database.
    fn test_project(
        repository_url: Option<&str>,
        forge_provisioned_at: Option<&str>,
        git_initialized_at: Option<&str>,
    ) -> projects::Project {
        projects::Project {
            id: uuid::Uuid::now_v7(),
            code: "acme".to_string(),
            name: "Acme matter".to_string(),
            status: "open".to_string(),
            brand: "neon".to_string(),
            entity_id: uuid::Uuid::now_v7(),
            firm_id: None,
            jurisdiction_id: None,
            client_dri_person_id: None,
            description: None,
            drive_folder_id: None,
            repository_url: repository_url.map(str::to_string),
            git_initialized_at: git_initialized_at.map(str::to_string),
            forge_provisioned_at: forge_provisioned_at.map(str::to_string),
            closed_at: None,
            closure_reason: None,
            internal_slack_channel_url: None,
            external_slack_channel_url: None,
            internal_slack_channel_id: None,
            private_notion_page_url: None,
            shared_notion_page_url: None,
            calendar_id: None,
            calendar_timezone: None,
            booking_url: None,
            onboarding_jurisdiction: None,
            legal_plan_id: None,
            plan_member_id: None,
            plan_case_number: None,
            benefit_code: None,
            plan_matter_type: None,
            plan_state: None,
            plan_assigned_on: None,
            plan_accepted_on: None,
            plan_closed_on: None,
            kit_version: None,
            inserted_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }
}
