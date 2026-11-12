use uuid::Uuid;

/// The response header `PATCH /app/api/projects/{id}` sets when the patch
/// names a Drive folder: [`DriveFolderCheck::as_str`] of the check it ran.
pub const DRIVE_FOLDER_CHECK_HEADER: &str = "navigator-drive-folder-check";

/// How far a Drive folder id named by a lawyer was confirmed before it was
/// recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveFolderCheck {
    /// The deployment's Drive provider found it as a live folder in the
    /// Firm's Projects shared drive.
    Verified,
    /// The deployment has no Drive provider, so only the id's shape was
    /// checked.
    Skipped,
}

impl DriveFolderCheck {
    /// The value carried by [`DRIVE_FOLDER_CHECK_HEADER`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Skipped => "skipped",
        }
    }
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
    /// Why Drive was skipped (the provider is disabled), or, for
    /// [`SurfaceStatus::Present`], that the folder already set on the
    /// Project was adopted rather than a new one created.
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
