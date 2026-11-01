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

use crate::persons;
use crate::projects::{self, set_drive_folder_id, ProjectCommandError, ProjectStoreError};
use crate::surreal::SurrealDb;

const DRIVE_CONFIGURATION_KEYS: [&str; 3] = [
    "NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID",
    "NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER",
    "NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON",
];

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

/// What a reconcile pass did about one provisioned surface (Drive folder or
/// source repository) — distinct from merely reporting the resulting value,
/// because a caller reading only the value cannot tell "this call did the
/// work" from "nothing needed doing" from "nothing was attempted".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceStatus {
    /// This call is what recorded the value: the Project row's column was
    /// null coming in, and the call created or adopted the external resource
    /// and wrote it.
    Created,
    /// Already recorded on the Project row before this call; nothing changed.
    Present,
    /// The Project row's column was null, but the resource already existed
    /// outside the store — a folder a person made by hand before opening
    /// the matter, named for the code. This call is what recorded it, but
    /// it did not create anything, so an operator reading the report can
    /// tell an adopted folder from a fresh one.
    Adopted,
    /// No Drive or forge service is configured for this deployment, so the
    /// surface was never attempted.
    Skipped,
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

/// What one reconcile pass recorded or confirmed.
///
/// Carries `serde::Deserialize` alongside `Serialize` so the CLI can parse
/// `POST /app/api/project-surfaces/{id}`'s body straight into this type — the
/// door and the client share one shape rather than two parsers agreeing by
/// convention.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectSurfaces {
    pub code: String,
    /// Always `projects/<code>/documents`. Not stored as its own column: the
    /// code is the prefix input, and a second field would be a second spelling
    /// to keep in step.
    pub documents_prefix: String,
    pub drive_folder_id: Option<String>,
    pub drive_status: SurfaceStatus,
    /// Present when Drive was skipped because the provider is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive_detail: Option<String>,
    pub repository_url: Option<String>,
    pub repository_status: SurfaceStatus,
    /// Present when repository provisioning was skipped because the forge is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_detail: Option<String>,
}

/// Nonsecret Project coordinates read for manifest fill and live gate checks.
/// DRI sides are vectors because the participation ledger permits more than
/// one accountable Person; a manifest value must match one live DRI.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectHandles {
    pub code: String,
    pub repository_url: Option<String>,
    pub lawyer_dri: Vec<String>,
    pub client_dri: Vec<String>,
    pub private_slack_channel: Option<String>,
    pub private_notion_page: Option<String>,
    /// The Project's shared Google Calendar id. `#[serde(default)]` so a
    /// response from a server that predates the column still deserializes.
    #[serde(default)]
    pub calendar_id: Option<String>,
    /// The Project's Drive ingest folder id, so `navigator project set
    /// --from-live` can fill `project.drive_folder_id` from the live row the
    /// same way it fills the calendar. `#[serde(default)]` for the same
    /// older-server reason.
    #[serde(default)]
    pub drive_folder_id: Option<String>,
    /// Person ids named by a DRI participation row (`is_lawyer_dri` or
    /// `is_client_dri`) that no longer resolve to a Person — the row survived
    /// a Person delete that should have cleared it (see
    /// `store::people_commands::delete_person`'s participation guard, which
    /// exists so this stays empty for every participation created from here
    /// on). [`read_handles`] skips these rather than failing the whole read,
    /// so one stale row can't 500 `navigator project set --from-live` or the
    /// Project gate for coordinates that are otherwise fine.
    /// `#[serde(default)]` so a caller reading an older cached response body
    /// still deserializes.
    #[serde(default)]
    pub dangling_dri_person_ids: Vec<Uuid>,
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
}

/// Read one Project's manifest coordinates without reconciling or writing any
/// external resource. The Project row remains authoritative for links; DRI
/// emails are derived from its participation markers and Person rows.
pub async fn read_handles(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<ProjectHandles, SurfaceError> {
    let project = projects::find_by_id(surreal, project_id)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?
        .ok_or(SurfaceError::NotFound)?;
    let participations = projects::participations_for_project(surreal, project_id)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?;
    let dri_ids: Vec<_> = participations
        .iter()
        .filter(|row| row.is_lawyer_dri || row.is_client_dri)
        .map(|row| row.person_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let people = crate::persons::find_by_ids(surreal, &dri_ids)
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
    Ok(ProjectHandles {
        code: project.code,
        repository_url: project.repository_url,
        lawyer_dri,
        client_dri,
        private_slack_channel: project.internal_slack_channel_url,
        private_notion_page: project.private_notion_page_url,
        calendar_id: project.calendar_id,
        drive_folder_id: project.drive_folder_id,
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
    surreal: &SurrealDb,
    project_id: Uuid,
    drive: Option<&D>,
    forge: Option<&F>,
) -> Result<ProjectSurfaces, SurfaceError>
where
    D: DriveService + ?Sized,
    F: ForgeService + ?Sized,
{
    let project = projects::find_by_id(surreal, project_id)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?
        .ok_or(SurfaceError::NotFound)?;

    let (drive_folder_id, drive_status, drive_detail) =
        if let Some(existing) = project.drive_folder_id.as_deref() {
            (Some(existing.to_string()), SurfaceStatus::Present, None)
        } else if let Some(drive) = drive {
            // LAW-213: a folder named for the code that predates the matter
            // is adopted, not shadowed by a second folder. Only a genuinely
            // absent folder is created.
            let (folder_id, status) =
                if let Some(existing) = drive.find_folder_by_name(&project.code).await? {
                    set_drive_folder_id(surreal, project_id, Some(&existing.id))
                        .await?
                        .ok_or(SurfaceError::NotFound)?;
                    (existing.id, SurfaceStatus::Adopted)
                } else {
                    let folder = drive.create_folder(&project.code).await?;
                    set_drive_folder_id(surreal, project_id, Some(&folder.id))
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
        grant_drive_ingest_membership(surreal, project_id, drive, folder_id).await?;
    }

    let (repository_url, repository_status, repository_detail) =
        if let Some(existing) = project.repository_url.as_deref() {
            (Some(existing.to_string()), SurfaceStatus::Present, None)
        } else if let Some(forge) = forge {
            let repository = forge.ensure_repository(&project.code).await?;
            record_provisioned_repository(surreal, project_id, &repository.url).await?;
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
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<ProjectSurfaces, SurfaceError> {
    let drive = if provider_configured(&DRIVE_CONFIGURATION_KEYS, |key| std::env::var(key).ok()) {
        Some(cloud::GoogleDrive::from_env().await?)
    } else {
        None
    };
    if drive.is_some() {
        let report = Box::pin(crate::project_folder_reconcile::reconcile_from_env(
            surreal, project_id,
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
            reconcile(surreal, project_id, drive.as_ref(), forge.as_ref()).await
        }
        cloud::ForgeBackend::Forgejo => {
            let kms = cloud::runtime_kms_from_env().await;
            let forge = forgejo_for_project(surreal, project_id, kms.as_ref(), lookup).await?;
            let root =
                lookup(cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV).unwrap_or_default();
            let seed = match scaffold_inputs(surreal, project_id).await {
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
                surreal,
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
    surreal: &SurrealDb,
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
    let surfaces = reconcile(surreal, project_id, drive, Some(forge)).await?;
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
        }
    }
}

/// Resolve the scaffold coordinates from the deployment environment and the
/// Project's participation.
async fn scaffold_inputs(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<ScaffoldInputs, SurfaceError> {
    let (lawyer_dri, client_dri) = dri_emails(surreal, project_id).await?;
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
    })
}

/// The first designated lawyer and client DRI emails for a Project — the
/// same derivation [`read_handles`] reports in full, narrowed to the one
/// email per side the scaffold manifest carries.
async fn dri_emails(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<(String, String), SurfaceError> {
    let participations = projects::participations_for_project(surreal, project_id)
        .await
        .map_err(|error| SurfaceError::Db(error.to_string()))?;
    let dri_ids: Vec<_> = participations
        .iter()
        .filter(|row| row.is_lawyer_dri || row.is_client_dri)
        .map(|row| row.person_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let people = crate::persons::find_by_ids(surreal, &dri_ids)
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
    surreal: &SurrealDb,
    project_id: Uuid,
    kms: &dyn cloud::RuntimeKms,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<cloud::ForgejoForge, SurfaceError> {
    let project = projects::find_by_id(surreal, project_id)
        .await?
        .ok_or(SurfaceError::NotFound)?;
    let root = lookup(cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV)
        .filter(|value| !value.trim().is_empty());
    let firm = match project.firm_id {
        Some(firm_id) => crate::firms::find_by_id(surreal, firm_id)
            .await
            .map_err(|error| SurfaceError::Db(error.to_string()))?,
        None => None,
    };
    let token = match firm.as_ref() {
        Some(firm) => crate::firm_secrets::resolve_firm_credential(
            surreal,
            firm.id,
            crate::firm_secrets::IntegrationProvider::Forgejo,
            crate::firm_secrets::IntegrationSecretKind::ForgejoAccessToken,
            kms,
        )
        .await
        .ok(),
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
        (Some(root), Some(firm), Some(token)) if missing.is_empty() => Ok(
            cloud::ForgejoForge::new(&root, &firm.slug, token.expose_for_provider_call())?,
        ),
        _ => Err(SurfaceError::ForgejoUnavailable(missing.join(", "))),
    }
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
pub async fn reconcile_after_open(surreal: &SurrealDb, project_id: Uuid) {
    if let Err(error) = Box::pin(reconcile_from_env(surreal, project_id)).await {
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
    surreal: &SurrealDb,
    project_id: Uuid,
    drive: &D,
    folder_id: &str,
) -> Result<(), SurfaceError>
where
    D: DriveService + ?Sized,
{
    let rows = projects::participations_for_project(surreal, project_id).await?;
    for row in rows {
        let Some(person) = persons::find_by_id(surreal, row.person_id)
            .await
            .map_err(|error| SurfaceError::Db(error.to_string()))?
        else {
            continue;
        };
        let viewer =
            crate::access::matter_viewer(surreal, Some(person.id), person.role, project_id)
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
    surreal: &SurrealDb,
    project_id: Uuid,
    url: &str,
) -> Result<(), SurfaceError> {
    projects::record_provisioned_repository(surreal, project_id, url, &Utc::now().to_rfc3339())
        .await?
        .ok_or(SurfaceError::NotFound)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        read_handles, reconcile, reconcile_forgejo, source_state, ForgeSite, ProjectSurfaces,
        ScaffoldInputs, SourceState, SurfaceError, SurfaceStatus,
    };
    use crate::persons::{self, NewPerson, Role};
    use crate::projects::{self, DriSide, OpenMatterCommand};
    use crate::test_support::{mem_surreal, seed_entity, seed_firm_with_brand};
    use cloud::drive::{DriveMemberKind, DriveRole, DriveService, FakeDrive};
    use cloud::forge::{FakeForge, ForgeError, ForgeRepository, ForgeService};
    use cloud::workspace::documents_prefix;

    #[test]
    fn partial_provider_configuration_is_not_treated_as_disabled() {
        let drive_keys = [
            "NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID",
            "NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER",
            "NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON",
        ];
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
        assert!(!super::provider_configured(&forge_keys, |_| None));
        assert!(super::provider_configured(&forge_keys, |key| {
            (key == "NAVIGATOR_GITHUB_ORG").then(|| "   ".to_string())
        }));
        assert!(super::provider_configured(&forge_keys, |key| {
            (key == "NAVIGATOR_GITHUB_APP_ID").then(|| "12345".to_string())
        }));
    }

    /// A forge that always refuses `ensure_repository` — for proving that a
    /// failed provisioning attempt leaves `forge_provisioned_at` (and
    /// `repository_url`) untouched rather than stamping a repository that
    /// was never actually recorded.
    struct FailingForge;

    #[async_trait::async_trait]
    impl ForgeService for FailingForge {
        async fn find_repository(
            &self,
            _project_code: &str,
        ) -> Result<Option<ForgeRepository>, ForgeError> {
            Ok(None)
        }

        async fn ensure_repository(
            &self,
            _project_code: &str,
        ) -> Result<ForgeRepository, ForgeError> {
            Err(ForgeError::Authentication)
        }

        async fn head_commit_sha(&self, _project_code: &str) -> Result<Option<String>, ForgeError> {
            Ok(None)
        }

        async fn head_commit_committed_at(
            &self,
            _project_code: &str,
        ) -> Result<Option<String>, ForgeError> {
            Ok(None)
        }

        async fn delete_repository(&self, _project_code: &str) -> Result<(), ForgeError> {
            Err(ForgeError::Authentication)
        }

        async fn read_file(
            &self,
            _project_code: &str,
            _path: &str,
        ) -> Result<Option<String>, ForgeError> {
            Ok(None)
        }
    }

    async fn open_acme(surreal: &crate::surreal::SurrealDb) -> projects::Project {
        seed_firm_with_brand(surreal, "neon").await;
        let entity_id = seed_entity(surreal).await;
        let lawyer = persons::create(
            surreal,
            &NewPerson::with_role("Lawyer", "lawyer@example.com", Role::Lawyer),
        )
        .await
        .expect("lawyer");
        let client = persons::create(
            surreal,
            &NewPerson::with_role("Client", "client@example.com", Role::Client),
        )
        .await
        .expect("client");
        projects::open_matter(
            surreal,
            &OpenMatterCommand {
                name: "Acme matter".into(),
                code: "acme".into(),
                client_id: client.id,
                entity_id,
                jurisdiction_id: None,
                description: None,
                brand: "neon".to_string(),
                attestation: true,
                acting_person_id: lawyer.id,
                closed_at: None,
            },
        )
        .await
        .expect("open")
    }

    #[tokio::test]
    async fn read_handles_returns_sorted_project_coordinates_without_writing() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let lawyer = persons::find_by_email_ci(&surreal, "lawyer@example.com")
            .await
            .expect("find lawyer")
            .expect("lawyer exists");
        let client = persons::find_by_email_ci(&surreal, "client@example.com")
            .await
            .expect("find client")
            .expect("client exists");
        projects::designate_dri_in_surreal(&surreal, project.id, lawyer.id, DriSide::Lawyer)
            .await
            .expect("designate lawyer DRI");
        projects::designate_dri_in_surreal(&surreal, project.id, client.id, DriSide::Client)
            .await
            .expect("designate client DRI");
        projects::set_repository_url(&surreal, project.id, Some("https://github.com/org/acme"))
            .await
            .expect("repository URL");
        projects::set_internal_slack_channel_url(
            &surreal,
            project.id,
            "https://slack.example.com/archives/private",
        )
        .await
        .expect("private Slack channel");
        projects::set_private_notion_page_url(
            &surreal,
            project.id,
            "https://notion.example.com/private",
        )
        .await
        .expect("private Notion page");

        let handles = read_handles(&surreal, project.id)
            .await
            .expect("read handles");

        assert_eq!(handles.code, "acme");
        assert_eq!(
            handles.repository_url.as_deref(),
            Some("https://github.com/org/acme")
        );
        assert_eq!(handles.lawyer_dri, ["lawyer@example.com"]);
        assert_eq!(handles.client_dri, ["client@example.com"]);
        assert_eq!(
            handles.private_slack_channel.as_deref(),
            Some("https://slack.example.com/archives/private")
        );
        assert_eq!(
            handles.private_notion_page.as_deref(),
            Some("https://notion.example.com/private")
        );
    }

    #[tokio::test]
    async fn read_handles_refuses_an_unknown_project() {
        let surreal = mem_surreal().await;
        let error = read_handles(&surreal, uuid::Uuid::now_v7())
            .await
            .expect_err("unknown Project cannot be read");
        assert!(matches!(error, SurfaceError::NotFound));
    }

    /// ENG-928: a DRI participation surviving the Person it names must not
    /// 500 this read — it must be skipped and reported, so
    /// `navigator project set --from-live` and the Project gate still see
    /// every other coordinate. `store::people_commands::delete_person` now
    /// refuses to create this state going forward; this proves the read
    /// side stays safe for a row that predates that guard (or one written
    /// straight to the store, as a migration or a hand-run query might).
    #[tokio::test]
    async fn read_handles_skips_a_dangling_dri_and_reports_it() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let lawyer = persons::find_by_email_ci(&surreal, "lawyer@example.com")
            .await
            .expect("find lawyer")
            .expect("lawyer exists");
        let client = persons::find_by_email_ci(&surreal, "client@example.com")
            .await
            .expect("find client")
            .expect("client exists");
        projects::designate_dri_in_surreal(&surreal, project.id, lawyer.id, DriSide::Lawyer)
            .await
            .expect("designate lawyer DRI");
        projects::designate_dri_in_surreal(&surreal, project.id, client.id, DriSide::Client)
            .await
            .expect("designate client DRI");

        // Delete the lawyer Person directly at the store layer — the same
        // shape a legacy row or a straight-to-store write left behind,
        // bypassing the command-layer guard that now refuses this.
        persons::delete(&surreal, lawyer.id)
            .await
            .expect("delete the lawyer Person out from under the DRI row");

        let handles = read_handles(&surreal, project.id)
            .await
            .expect("a dangling DRI must not fail the whole read");

        assert!(
            handles.lawyer_dri.is_empty(),
            "the dangling lawyer DRI must not appear as an email: {:?}",
            handles.lawyer_dri
        );
        assert_eq!(
            handles.client_dri,
            ["client@example.com"],
            "the client DRI, which is still a real Person, must read through untouched"
        );
        assert_eq!(
            handles.dangling_dri_person_ids,
            vec![lawyer.id],
            "the dangling participation must be named, not silently dropped"
        );
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

    #[tokio::test]
    async fn reconcile_creates_the_drive_folder_and_the_repository() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let drive = FakeDrive::default();
        let forge = FakeForge::new();

        let surfaces = reconcile(&surreal, project.id, Some(&drive), Some(&forge))
            .await
            .expect("reconcile");

        assert_eq!(
            surfaces,
            ProjectSurfaces {
                code: project.code.clone(),
                documents_prefix: format!("projects/{}/documents", project.code),
                drive_folder_id: Some("folder-1".into()),
                drive_status: SurfaceStatus::Created,
                drive_detail: None,
                repository_url: Some(format!(
                    "https://forge.example/an-organization/{}",
                    project.code
                )),
                repository_status: SurfaceStatus::Created,
                repository_detail: None,
            }
        );

        let reloaded = projects::find_by_id(&surreal, project.id)
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(reloaded.drive_folder_id.as_deref(), Some("folder-1"));
        assert_eq!(
            reloaded.repository_url.as_deref(),
            Some(format!("https://forge.example/an-organization/{}", project.code).as_str())
        );
        assert!(reloaded.forge_provisioned_at.is_some());

        let members = drive.members("folder-1");
        let emails: Vec<&str> = members
            .iter()
            .map(|(member, role)| {
                assert_eq!(member.kind, DriveMemberKind::User);
                assert_eq!(*role, DriveRole::Writer);
                member.email.as_str()
            })
            .collect();
        assert!(emails.contains(&"lawyer@example.com"), "{emails:?}");
        assert!(
            !emails.contains(&"client@example.com"),
            "clients never receive a working-root grant: {emails:?}"
        );
    }

    #[tokio::test]
    async fn a_second_reconcile_adopts_and_does_not_duplicate() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let drive = FakeDrive::default();
        let forge = FakeForge::new();

        let first = reconcile(&surreal, project.id, Some(&drive), Some(&forge))
            .await
            .expect("first");
        let second = reconcile(&surreal, project.id, Some(&drive), Some(&forge))
            .await
            .expect("second");

        assert_eq!(first.drive_folder_id, second.drive_folder_id);
        assert_eq!(first.repository_url, second.repository_url);
        assert_eq!(first.drive_status, SurfaceStatus::Created);
        assert_eq!(first.repository_status, SurfaceStatus::Created);
        assert_eq!(
            second.drive_status,
            SurfaceStatus::Present,
            "the second pass found the folder already recorded rather than creating it again"
        );
        assert_eq!(second.repository_status, SurfaceStatus::Present);
        assert_eq!(forge.repository_count(), 1);
        let folders = drive.list_folders().await.expect("list");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, project.code);
    }

    #[tokio::test]
    async fn an_existing_drive_folder_is_adopted_not_recreated() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let drive = FakeDrive::default();
        let already = drive
            .create_folder(&project.code)
            .await
            .expect("pre-create");
        let forge = FakeForge::new();

        let surfaces = reconcile(&surreal, project.id, Some(&drive), Some(&forge))
            .await
            .expect("adopt");
        assert_eq!(
            surfaces.drive_folder_id.as_deref(),
            Some(already.id.as_str())
        );
        assert_eq!(
            surfaces.drive_status,
            SurfaceStatus::Adopted,
            "adopting an untracked external folder is reported as its own outcome, \
             distinct from a fresh create, so `project setup` never claims to \
             have made a folder it only found"
        );
        let folders = drive.list_folders().await.expect("list");
        assert_eq!(folders.len(), 1);

        // Adoption records the pre-existing folder on the Project row, so a
        // later reconcile takes the "present" branch and never touches Drive.
        let reloaded = projects::find_by_id(&surreal, project.id)
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(
            reloaded.drive_folder_id.as_deref(),
            Some(already.id.as_str())
        );
    }

    #[tokio::test]
    async fn a_recorded_repository_url_is_left_alone() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        projects::set_repository_url(
            &surreal,
            project.id,
            Some("https://git.example.internal/client-org/acme"),
        )
        .await
        .expect("record a URL on another forge");
        let drive = FakeDrive::default();
        let forge = FakeForge::new();

        let surfaces = reconcile(&surreal, project.id, Some(&drive), Some(&forge))
            .await
            .expect("leave recorded URL");
        assert_eq!(
            surfaces.repository_url.as_deref(),
            Some("https://git.example.internal/client-org/acme")
        );
        assert_eq!(surfaces.repository_status, SurfaceStatus::Present);
        assert_eq!(forge.repository_count(), 0);
    }

    #[tokio::test]
    async fn missing_services_skip_those_surfaces() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let surfaces = reconcile(&surreal, project.id, None::<&FakeDrive>, None::<&FakeForge>)
            .await
            .expect("skip");
        assert_eq!(
            surfaces.documents_prefix,
            format!("projects/{}/documents", project.code)
        );
        assert_eq!(surfaces.drive_folder_id, None);
        assert_eq!(surfaces.drive_status, SurfaceStatus::Skipped);
        assert_eq!(
            surfaces.drive_detail.as_deref(),
            Some("Drive provider is not configured")
        );
        assert_eq!(surfaces.repository_url, None);
        assert_eq!(surfaces.repository_status, SurfaceStatus::Skipped);
        assert_eq!(
            surfaces.repository_detail.as_deref(),
            Some("repository provider is not configured")
        );
    }

    #[tokio::test]
    async fn unknown_project_is_not_found() {
        let surreal = mem_surreal().await;
        let error = reconcile(
            &surreal,
            uuid::Uuid::now_v7(),
            None::<&FakeDrive>,
            None::<&FakeForge>,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, SurfaceError::NotFound));
    }

    #[tokio::test]
    async fn a_failed_forge_attempt_stamps_nothing() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let forge = FailingForge;

        let error = reconcile(&surreal, project.id, None::<&FakeDrive>, Some(&forge))
            .await
            .unwrap_err();
        assert!(matches!(error, SurfaceError::Forge(_)));

        let reloaded = projects::find_by_id(&surreal, project.id)
            .await
            .expect("load")
            .expect("exists");
        assert_eq!(reloaded.repository_url, None);
        assert_eq!(
            reloaded.forge_provisioned_at, None,
            "a failed create/adopt attempt must never stamp forge_provisioned_at"
        );
    }

    #[tokio::test]
    async fn a_retry_never_moves_the_first_success_stamp() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let forge = FakeForge::new();

        reconcile(&surreal, project.id, None::<&FakeDrive>, Some(&forge))
            .await
            .expect("first reconcile stamps forge_provisioned_at");
        let first_stamp = projects::find_by_id(&surreal, project.id)
            .await
            .expect("load")
            .expect("exists")
            .forge_provisioned_at
            .expect("stamped on first success");

        // A second reconcile finds `repository_url` already recorded and
        // takes the "present" branch, never the forge, so nothing re-stamps.
        reconcile(&surreal, project.id, None::<&FakeDrive>, Some(&forge))
            .await
            .expect("second reconcile is a no-op adopt");
        let second_stamp = projects::find_by_id(&surreal, project.id)
            .await
            .expect("load")
            .expect("exists")
            .forge_provisioned_at
            .expect("still stamped");

        assert_eq!(first_stamp, second_stamp);
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
            booking_url: None,
            onboarding_jurisdiction: None,
            inserted_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// The ForgeSite the `FakeForge` answers for: its own host and
    /// organization, so a recorded fake URL passes the ownership check.
    fn forge_site() -> ForgeSite<'static> {
        ForgeSite {
            root: "https://forge.example",
            owner: "an-organization",
        }
    }

    /// Scaffold coordinates matching what the Acme matter derives: a
    /// deployment host so seeding engages, this workspace's version, and no
    /// designated DRIs, which the manifest carries as empty strings.
    fn seed_inputs() -> ScaffoldInputs {
        ScaffoldInputs {
            host: Some("staging.neonlaw.com".to_string()),
            version: env!("CARGO_PKG_VERSION").to_string(),
            lawyer_dri: String::new(),
            client_dri: String::new(),
        }
    }

    #[tokio::test]
    async fn a_fresh_forgejo_repository_is_seeded_then_governed() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let drive = FakeDrive::default();
        let forge = FakeForge::default();

        let surfaces = reconcile_forgejo(
            &surreal,
            project.id,
            Some(&drive),
            &forge,
            forge_site(),
            &seed_inputs(),
        )
        .await
        .expect("reconcile");

        assert_eq!(surfaces.repository_status, SurfaceStatus::Created);
        assert_eq!(
            forge.initialize_writes(),
            1,
            "the canonical scaffold is exactly one first commit"
        );
        assert_eq!(
            forge.governance_reconciles(),
            1,
            "governance follows seeding"
        );
        assert_eq!(
            forge.call_log(),
            vec![
                ("ensure", project.code.clone()),
                ("initialize", project.code.clone()),
                ("govern", project.code.clone()),
            ],
            "the scaffold is written to main before the protection that would refuse it"
        );
        let source = project_scaffold::ScaffoldSource {
            code: &project.code,
            host: "staging.neonlaw.com",
            lawyer_dri: "",
            client_dri: "",
            version: env!("CARGO_PKG_VERSION"),
        };
        let mut expected: Vec<String> = project_scaffold::forgejo_seed_files(&source)
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        expected.sort_unstable();
        assert_eq!(forge.committed_paths(&project.code), expected);
    }

    #[tokio::test]
    async fn a_lost_seed_reply_writes_nothing_twice_and_still_governs() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let drive = FakeDrive::default();
        let forge = FakeForge::default();
        forge.lose_next_write_reply();

        let first = reconcile_forgejo(
            &surreal,
            project.id,
            Some(&drive),
            &forge,
            forge_site(),
            &seed_inputs(),
        )
        .await
        .expect("the provision stands even though the seed reply was lost");
        assert_eq!(first.repository_status, SurfaceStatus::Created);
        assert_eq!(
            forge.initialize_writes(),
            1,
            "the write landed before the reply was lost"
        );
        assert_eq!(
            forge.governance_reconciles(),
            1,
            "governance still followed"
        );

        let second = reconcile_forgejo(
            &surreal,
            project.id,
            Some(&drive),
            &forge,
            forge_site(),
            &seed_inputs(),
        )
        .await
        .expect("retry");
        assert_eq!(second.repository_status, SurfaceStatus::Present);
        assert_eq!(
            forge.initialize_writes(),
            1,
            "the retry writes nothing twice: the head already binds the digest"
        );
        assert_eq!(forge.governance_reconciles(), 2);
    }

    #[tokio::test]
    async fn an_adopted_repository_is_governed_and_its_drift_repaired() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let drive = FakeDrive::default();
        let forge = FakeForge::default();
        forge
            .ensure_repository(&project.code)
            .await
            .expect("forge-level repository");
        forge.set_governance_drift(
            &project.code,
            &["main.enable_push", "repository.allow_squash_merge"],
        );

        let surfaces = reconcile_forgejo(
            &surreal,
            project.id,
            Some(&drive),
            &forge,
            forge_site(),
            &seed_inputs(),
        )
        .await
        .expect("reconcile");

        assert_eq!(
            surfaces.repository_status,
            SurfaceStatus::Created,
            "a forge-level repository the Project row never recorded is adopted this pass"
        );
        assert_eq!(forge.governance_reconciles(), 1, "adoption is governed too");
        assert_eq!(
            forge.initialize_writes(),
            1,
            "the adopted repository is empty, so the scaffold seeds it"
        );
    }

    #[tokio::test]
    async fn a_recorded_url_on_another_forge_is_never_touched() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let drive = FakeDrive::default();
        let forge = FakeForge::default();
        projects::set_repository_url(
            &surreal,
            project.id,
            Some("https://git.example.internal/client-org/acme"),
        )
        .await
        .expect("repository URL");

        let surfaces = reconcile_forgejo(
            &surreal,
            project.id,
            Some(&drive),
            &forge,
            forge_site(),
            &seed_inputs(),
        )
        .await
        .expect("reconcile");

        assert_eq!(surfaces.repository_status, SurfaceStatus::Present);
        assert_eq!(
            forge.governance_reconciles(),
            0,
            "a matter whose source deliberately lives elsewhere keeps it"
        );
        assert_eq!(forge.initialize_writes(), 0);
        assert!(forge.call_log().is_empty());
    }

    #[tokio::test]
    async fn an_empty_recorded_repository_recovers_when_the_coordinates_arrive() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let drive = FakeDrive::default();
        let forge = FakeForge::default();
        let no_host = ScaffoldInputs {
            host: None,
            ..seed_inputs()
        };

        let first = reconcile_forgejo(
            &surreal,
            project.id,
            Some(&drive),
            &forge,
            forge_site(),
            &no_host,
        )
        .await
        .expect("reconcile without deployment coordinates");
        assert_eq!(first.repository_status, SurfaceStatus::Created);
        assert_eq!(
            forge.initialize_writes(),
            0,
            "without the host the manifest's gate audience would be a lie"
        );
        assert_eq!(forge.governance_reconciles(), 1, "governance needs no host");

        let second = reconcile_forgejo(
            &surreal,
            project.id,
            Some(&drive),
            &forge,
            forge_site(),
            &seed_inputs(),
        )
        .await
        .expect("reconcile with coordinates");
        assert_eq!(second.repository_status, SurfaceStatus::Present);
        assert_eq!(
            forge.initialize_writes(),
            1,
            "the recorded empty repository is seeded on a later pass"
        );
    }

    #[tokio::test]
    async fn a_repository_holding_other_source_is_preserved_and_still_governed() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let drive = FakeDrive::default();
        let forge = FakeForge::default();
        let mut foreign = std::collections::BTreeMap::new();
        foreign.insert("attorney-notes.md".to_string(), "hand-written".to_string());
        // The attorney's repository already exists on the forge, holding
        // source Navigator never wrote; register it before committing to it.
        forge
            .ensure_repository(&project.code)
            .await
            .expect("pre-existing repository");
        forge
            .initialize_source(
                &project.code,
                &foreign,
                "not-the-canonical-digest",
                "Attorney notes",
            )
            .await
            .expect("foreign source");

        let surfaces = reconcile_forgejo(
            &surreal,
            project.id,
            Some(&drive),
            &forge,
            forge_site(),
            &seed_inputs(),
        )
        .await
        .expect("reconcile");

        assert_eq!(surfaces.repository_status, SurfaceStatus::Created);
        assert_eq!(
            forge.initialize_writes(),
            1,
            "only the attorney's own first commit ever wrote"
        );
        assert_eq!(forge.governance_reconciles(), 1, "still governed");
        assert_eq!(
            forge
                .read_file(&project.code, "attorney-notes.md")
                .await
                .expect("read"),
            Some("hand-written".to_string()),
            "the existing source is preserved, never replaced"
        );
    }

    /// A governed repository swept again is not re-protected unnecessarily:
    /// the second pass re-locks governance (finding no drift to repair),
    /// re-derives the scaffold digest and writes nothing, and never ensures
    /// a second repository — the whole point of a repeated backfill run.
    #[tokio::test]
    async fn an_already_governed_repository_is_not_reprotected_on_the_next_pass() {
        let surreal = mem_surreal().await;
        let project = open_acme(&surreal).await;
        let drive = FakeDrive::default();
        let forge = FakeForge::default();

        let first = reconcile_forgejo(
            &surreal,
            project.id,
            Some(&drive),
            &forge,
            forge_site(),
            &seed_inputs(),
        )
        .await
        .expect("first pass");
        assert_eq!(first.repository_status, SurfaceStatus::Created);

        let second = reconcile_forgejo(
            &surreal,
            project.id,
            Some(&drive),
            &forge,
            forge_site(),
            &seed_inputs(),
        )
        .await
        .expect("second pass");

        assert_eq!(second.repository_status, SurfaceStatus::Present);
        assert_eq!(
            second.repository_url, first.repository_url,
            "the URL never moves"
        );
        assert_eq!(
            forge.repository_count(),
            1,
            "no second repository is invented"
        );
        assert_eq!(
            forge.ensure_calls(),
            1,
            "a recorded repository is not re-ensured"
        );
        assert_eq!(
            forge.initialize_writes(),
            1,
            "the scaffold is not rewritten: the head already binds the digest"
        );
        assert_eq!(
            forge.governance_reconciles(),
            2,
            "governance is re-locked on every pass"
        );
        assert_eq!(
            forge.call_log(),
            vec![
                ("ensure", project.code.clone()),
                ("initialize", project.code.clone()),
                ("govern", project.code.clone()),
                ("initialize", project.code.clone()),
                ("govern", project.code.clone()),
            ],
            "the second pass re-derives the digest and re-locks governance, but writes nothing"
        );
    }
}
