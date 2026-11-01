//! Resumable provider-folder provisioning with verified publication boundaries.
use crate::{
    document_pointers::MatterFolder,
    postgres::PostgresDb,
    project_folders::{self, ProjectFolder, ProjectFolderSlot},
    surreal::SurrealDb,
};
use cloud::drive::{
    DriveFolderAccess, DriveIsolationOutcome, DriveMember, DriveMemberKind, DriveRole, DriveService,
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

static CONCURRENCY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

/// Provider coordinates belong in runtime configuration, never seeds or source.
#[derive(Clone)]
pub struct FolderProvisioningPolicy {
    pub projects_parent_id: String,
    /// Service identities and organizers explicitly trusted for this workspace.
    pub trusted_organizers: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderBlock {
    UnsupportedProvider,
    ProviderCapability,
    MissingFirmParticipant,
    UntrustedPermission,
    UntrustedOrganizer,
    ParentMismatch,
}
impl FolderBlock {
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::UnsupportedProvider => "Drive provider cannot inspect restricted folders",
            Self::ProviderCapability => "Drive provider could not confirm restricted access",
            Self::MissingFirmParticipant => "the matter has no authorized firm participant",
            Self::UntrustedPermission => {
                "the folder has a grant outside the authorized participant set"
            }
            Self::UntrustedOrganizer => {
                "the Drive organizer cannot be verified as a trusted firm identity"
            }
            Self::ParentMismatch => "the folder is outside its reserved parent",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderOutcome {
    Complete,
    Blocked(FolderBlock),
}
#[derive(Debug, Clone)]
pub struct FolderReconcileReport {
    pub project_id: Uuid,
    pub outcome: FolderOutcome,
    /// Only populated after every required slot is verified.
    pub client_submissions_id: Option<String>,
}
#[derive(Debug, thiserror::Error)]
pub enum FolderReconcileError {
    #[error("Project storage projection failed")]
    Projection(#[from] crate::postgres_mirror::MirrorError),
    #[error("folder binding: {0}")]
    Binding(#[from] project_folders::ProjectFolderError),
    #[error("folder provider: {0}")]
    Provider(#[from] cloud::drive::DriveError),
    #[error("folder transaction failed")]
    Database(#[from] sqlx::Error),
    #[error("matter lookup failed")]
    Matter,
    #[error("folder PostgreSQL configuration is unavailable")]
    PostgresUnavailable,
    #[error("folder policy blocked: {0:?}")]
    PolicyBlocked(FolderBlock),
}
#[derive(Default)]
struct Members {
    firm: BTreeSet<String>,
    client: BTreeSet<String>,
    trusted: BTreeSet<String>,
}
fn email(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}
async fn members(
    db: &SurrealDb,
    project_id: Uuid,
    policy: &FolderProvisioningPolicy,
) -> Result<Members, FolderReconcileError> {
    let mut result = Members {
        trusted: policy.trusted_organizers.iter().map(|v| email(v)).collect(),
        ..Members::default()
    };
    for participation in crate::projects::participations_for_project(db, project_id)
        .await
        .map_err(|_| FolderReconcileError::Matter)?
    {
        let Some(person) = crate::persons::find_by_id(db, participation.person_id)
            .await
            .map_err(|_| FolderReconcileError::Matter)?
        else {
            continue;
        };
        let Some(viewer) =
            crate::access::matter_viewer(db, Some(person.id), person.role, project_id)
                .await
                .map_err(|_| FolderReconcileError::Matter)?
        else {
            continue;
        };
        let address = email(&person.email);
        if address.is_empty() {
            continue;
        }
        if viewer.is_firm_side() {
            result.firm.insert(address);
        } else if matches!(
            viewer,
            crate::access::MatterViewer::Client | crate::access::MatterViewer::ClientDri
        ) {
            result.client.insert(address);
        }
    }
    result.trusted.extend(result.firm.iter().cloned());
    Ok(result)
}

/// Validate organizer and direct grants independently of inherited-access state.
fn inspect(
    access: &DriveFolderAccess,
    members: &Members,
    _clients_allowed: bool,
) -> Result<(), FolderBlock> {
    for grant in &access.permissions {
        let address = grant.email.as_deref().map(email);
        if grant.role == "organizer" {
            if grant.permission_type != "user"
                || !address
                    .as_ref()
                    .is_some_and(|v| members.trusted.contains(v) && !members.client.contains(v))
            {
                return Err(FolderBlock::UntrustedOrganizer);
            }
        } else if grant.direct {
            let Some(address) = address else {
                return Err(FolderBlock::UntrustedPermission);
            };
            if grant.permission_type != "user"
                || !(members.firm.contains(&address)
                    || members.trusted.contains(&address)
                    || members.client.contains(&address))
            {
                return Err(FolderBlock::UntrustedPermission);
            }
        } else if !grant.inherited {
            return Err(FolderBlock::UntrustedPermission);
        }
    }
    Ok(())
}

/// No organizer or shared-drive membership is removed here.
// Preserve the ordered provider audit and its fail-closed publication boundary.
#[allow(clippy::too_many_lines)]
async fn secure<D: DriveService + ?Sized>(
    drive: &D,
    id: &str,
    members: &Members,
    clients_allowed: bool,
    may_remove_clients: bool,
) -> Result<Result<(), FolderBlock>, FolderReconcileError> {
    let before = match drive.folder_access(id).await {
        Ok(access) => access,
        Err(cloud::drive::DriveError::UnsupportedTree) => {
            return Ok(Err(FolderBlock::UnsupportedProvider));
        }
        Err(error) => return Err(error.into()),
    };
    if let Err(block) = inspect(&before, members, clients_allowed) {
        return Ok(Err(block));
    }
    if !clients_allowed
        && !may_remove_clients
        && before.permissions.iter().any(|g| {
            g.direct
                && g.email
                    .as_deref()
                    .is_some_and(|e| members.client.contains(&email(e)))
        })
    {
        return Ok(Err(FolderBlock::ProviderCapability));
    }
    let allowed: BTreeSet<_> = members
        .firm
        .iter()
        .chain(members.trusted.iter())
        .chain(
            clients_allowed
                .then_some(&members.client)
                .into_iter()
                .flatten(),
        )
        .collect();
    for address in allowed {
        drive
            .ensure_direct_member_permission(
                id,
                &DriveMember {
                    kind: DriveMemberKind::User,
                    email: address.clone(),
                },
                DriveRole::Writer,
            )
            .await?;
    }
    let restricted = match drive.restrict_folder(id).await? {
        DriveIsolationOutcome::Restricted(access) => access,
        DriveIsolationOutcome::Blocked(_) => return Ok(Err(FolderBlock::ProviderCapability)),
    };
    if !restricted.inherited_permissions_disabled || !restricted.can_list_children {
        return Ok(Err(FolderBlock::ProviderCapability));
    }
    if let Err(block) = inspect(&restricted, members, clients_allowed) {
        return Ok(Err(block));
    }
    if !clients_allowed {
        for grant in restricted
            .permissions
            .iter()
            .filter(|g| g.direct && g.role != "organizer")
        {
            if let Some(address) = grant
                .email
                .as_deref()
                .map(email)
                .filter(|v| members.client.contains(v))
            {
                drive
                    .remove_member_permission(
                        id,
                        &DriveMember {
                            kind: DriveMemberKind::User,
                            email: address,
                        },
                    )
                    .await?;
            }
        }
    }
    let after = drive.folder_access(id).await?;
    if !after.inherited_permissions_disabled || !after.can_list_children {
        return Ok(Err(FolderBlock::ProviderCapability));
    }
    if let Err(block) = inspect(&after, members, clients_allowed) {
        return Ok(Err(block));
    }
    if let Err(block) = inspect_content_grants(&after, members, clients_allowed) {
        return Ok(Err(block));
    }
    let direct: BTreeSet<_> = after
        .permissions
        .iter()
        .filter(|g| (g.direct || g.role == "organizer") && g.permission_type == "user")
        .filter_map(|g| g.email.as_deref().map(email))
        .collect();
    if !members.firm.iter().all(|v| direct.contains(v))
        || (clients_allowed && !members.client.iter().all(|v| direct.contains(v)))
        || (!clients_allowed && members.client.iter().any(|v| direct.contains(v)))
    {
        return Ok(Err(FolderBlock::ProviderCapability));
    }
    Ok(Ok(()))
}

/// Audit file-specific grants; Google limited access applies to folders, not files.
async fn secure_file<D: DriveService + ?Sized>(
    drive: &D,
    id: &str,
    members: &Members,
    clients_allowed: bool,
) -> Result<Result<(), FolderBlock>, FolderReconcileError> {
    let permissions = drive.item_permissions(id).await?;
    let view = |permissions| DriveFolderAccess {
        inherited_permissions_disabled: true,
        can_disable_inheritance: false,
        can_enable_inheritance: true,
        can_list_children: true,
        parent_ids: Vec::new(),
        permissions,
    };
    if let Err(block) = inspect(&view(permissions.clone()), members, clients_allowed) {
        return Ok(Err(block));
    }
    for grant in permissions
        .iter()
        .filter(|g| !clients_allowed && g.direct && g.role != "organizer")
    {
        if let Some(address) = grant
            .email
            .as_deref()
            .map(email)
            .filter(|v| members.client.contains(v))
        {
            drive
                .remove_member_permission(
                    id,
                    &DriveMember {
                        kind: DriveMemberKind::User,
                        email: address,
                    },
                )
                .await?;
        }
    }
    let after = drive.item_permissions(id).await?;
    if let Err(block) = inspect_content_grants(&view(after.clone()), members, clients_allowed) {
        return Ok(Err(block));
    }
    if let Err(block) = inspect(&view(after.clone()), members, clients_allowed) {
        return Ok(Err(block));
    }
    if !clients_allowed
        && after.iter().any(|g| {
            g.direct
                && g.email
                    .as_deref()
                    .is_some_and(|e| members.client.contains(&email(e)))
        })
    {
        return Ok(Err(FolderBlock::ProviderCapability));
    }
    Ok(Ok(()))
}

fn inspect_content_grants(
    access: &DriveFolderAccess,
    members: &Members,
    clients_allowed: bool,
) -> Result<(), FolderBlock> {
    for grant in &access.permissions {
        if grant.view.as_deref() == Some("metadata") {
            continue;
        }
        if grant.role == "organizer" {
            continue;
        } // inspect() already verifies organizer identity.
        if grant.inherited
            && !grant
                .email
                .as_deref()
                .map(email)
                .as_ref()
                .is_some_and(|id| {
                    members.firm.contains(id)
                        || members.trusted.contains(id)
                        || (clients_allowed && members.client.contains(id))
                })
        {
            return Err(FolderBlock::UntrustedPermission);
        }
    }
    Ok(())
}

fn inspect_descendant(
    access: &DriveFolderAccess,
    members: &Members,
    clients_allowed: bool,
) -> Result<(), FolderBlock> {
    if access.inherited_permissions_disabled || !access.can_list_children {
        return Err(FolderBlock::ProviderCapability);
    }
    let mut content = access.clone();
    content
        .permissions
        .retain(|g| g.view.as_deref() != Some("metadata"));
    inspect(&content, members, clients_allowed)?;
    let allowed = |address: &String| {
        members.firm.contains(address)
            || members.trusted.contains(address)
            || (clients_allowed && members.client.contains(address))
    };
    if content.permissions.iter().any(|g| {
        g.role != "organizer"
            && (!g.inherited || !g.email.as_deref().map(email).as_ref().is_some_and(allowed))
    }) {
        return Err(FolderBlock::UntrustedPermission);
    }
    let inherited: BTreeSet<_> = content
        .permissions
        .iter()
        .filter(|g| g.inherited || g.role == "organizer")
        .filter_map(|g| g.email.as_deref().map(email))
        .collect();
    if !members.firm.iter().all(|id| inherited.contains(id))
        || (clients_allowed && !members.client.iter().all(|id| inherited.contains(id)))
    {
        return Err(FolderBlock::ProviderCapability);
    }
    Ok(())
}
async fn secure_descendant<D: DriveService + ?Sized>(
    drive: &D,
    id: &str,
    members: &Members,
    clients_allowed: bool,
) -> Result<Result<(), FolderBlock>, FolderReconcileError> {
    let before = drive.folder_access(id).await?;
    if let Err(block) = inspect(&before, members, clients_allowed) {
        return Ok(Err(block));
    }
    let Some(inherited) = drive.inherit_folder(id).await? else {
        return Ok(Err(FolderBlock::ProviderCapability));
    };
    // Known direct grants are redundant or unsafe once the root owns access.
    // Organizers are never changed; unknown grants were refused before any write.
    for grant in inherited
        .permissions
        .iter()
        .filter(|g| g.direct && g.role != "organizer")
    {
        if let Some(address) = grant.email.as_deref().map(email) {
            drive
                .remove_member_permission(
                    id,
                    &DriveMember {
                        kind: DriveMemberKind::User,
                        email: address,
                    },
                )
                .await?;
        }
    }
    Ok(inspect_descendant(
        &drive.folder_access(id).await?,
        members,
        clients_allowed,
    ))
}

async fn ensure_slot<D: DriveService + ?Sized>(
    pg: &PostgresDb,
    drive: &D,
    project_id: Uuid,
    slot: ProjectFolderSlot,
    parent: Option<&ProjectFolder>,
    parent_drive: &str,
    name: &str,
) -> Result<ProjectFolder, FolderReconcileError> {
    let row = if let Some(row) = project_folders::get(pg, project_id, slot).await? {
        row
    } else {
        let id = if let Some(existing) = drive.find_child_folder(parent_drive, name).await? {
            existing.id
        } else {
            drive.allocate_folder_id().await?
        };
        // A reservation is committed before the provider create. Crash recovery retries this id.
        project_folders::bind(
            pg,
            project_id,
            slot,
            &id,
            parent.map(|p| p.id),
            Some(parent_drive),
        )
        .await?
    };
    project_folders::clear_verification(pg, project_id, slot, &row.drive_folder_id).await?;
    drive
        .ensure_child_folder_with_id(parent_drive, name, &row.drive_folder_id)
        .await?;
    Ok(row)
}

/// Reconcile a Project under one transaction-scoped advisory lane.
///
/// # Errors
/// Provider/store failures remain retryable; policy blocks are returned in the report.
// Preserve the ordered provider audit and its fail-closed publication boundary.
#[allow(clippy::too_many_lines)]
pub async fn reconcile<D: DriveService + ?Sized>(
    db: &SurrealDb,
    pg: &PostgresDb,
    drive: &D,
    project_id: Uuid,
    policy: &FolderProvisioningPolicy,
) -> Result<FolderReconcileReport, FolderReconcileError> {
    let blocked = |block| FolderReconcileReport {
        project_id,
        outcome: FolderOutcome::Blocked(block),
        client_submissions_id: None,
    };
    // Each lane pins one pooled connection while binding operations need another.
    // Bound this process below the application's ten-connection pool size.
    let _permit = CONCURRENCY
        .acquire()
        .await
        .map_err(|_| FolderReconcileError::Matter)?;
    let mut lane = pg.pool().begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '5s'")
        .execute(&mut *lane)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 167))")
        .bind(project_id.to_string())
        .execute(&mut *lane)
        .await?;
    project_folders::clear_project_verification(pg, project_id).await?;
    let project = crate::projects::find_by_id(db, project_id)
        .await
        .map_err(|_| FolderReconcileError::Matter)?
        .ok_or(FolderReconcileError::Matter)?;
    // Project Folder is PostgreSQL-authoritative while Projects are still
    // Surreal-authoritative. Seed its FK through the canonical projection even
    // when optional automatic mirroring is disabled or historical rows are absent.
    if let Some(firm_id) = project.firm_id {
        crate::postgres_mirror::mirror(db, pg, &crate::postgres_mirror::MirrorKey::Firm(firm_id))
            .await?;
    }
    if let Some(jurisdiction_id) = project.jurisdiction_id {
        crate::postgres_mirror::mirror(
            db,
            pg,
            &crate::postgres_mirror::MirrorKey::Jurisdiction(jurisdiction_id),
        )
        .await?;
    }
    crate::postgres_mirror::mirror(
        db,
        pg,
        &crate::postgres_mirror::MirrorKey::Project(project_id),
    )
    .await?;
    let members = members(db, project_id, policy).await?;
    if members.firm.is_empty() {
        return Ok(blocked(FolderBlock::MissingFirmParticipant));
    }
    let root = if let Some(id) = project.drive_folder_id.as_deref() {
        let access = drive.folder_access(id).await?;
        if access.parent_ids.len() != 1
            || access.parent_ids.first() != Some(&policy.projects_parent_id)
        {
            return Ok(blocked(FolderBlock::ParentMismatch));
        }
        project_folders::bind(
            pg,
            project_id,
            ProjectFolderSlot::Root,
            id,
            None,
            Some(&policy.projects_parent_id),
        )
        .await?
    } else {
        ensure_slot(
            pg,
            drive,
            project_id,
            ProjectFolderSlot::Root,
            None,
            &policy.projects_parent_id,
            &project.code,
        )
        .await?
    };
    let submissions = ensure_slot(
        pg,
        drive,
        project_id,
        ProjectFolderSlot::ClientSubmissions,
        None,
        &policy.projects_parent_id,
        &format!("{} - Client Submissions", project.code),
    )
    .await?;
    // Establish verified alternative client intake before removing a direct root grant.
    if let Err(block) = secure(drive, &submissions.drive_folder_id, &members, true, false).await? {
        return Ok(blocked(block));
    }
    if let Err(block) = secure(drive, &root.drive_folder_id, &members, false, true).await? {
        return Ok(blocked(block));
    }
    let mut rows = BTreeMap::new();
    rows.insert(ProjectFolderSlot::Root.key(), root.clone());
    for folder in MatterFolder::TREE {
        let parent_slot = if folder.is_work_product_subfolder() {
            ProjectFolderSlot::Matter(MatterFolder::WorkProduct)
        } else {
            ProjectFolderSlot::Root
        };
        let parent = rows
            .get(parent_slot.key())
            .ok_or(FolderReconcileError::Matter)?;
        let name = folder
            .as_str()
            .rsplit('/')
            .next()
            .ok_or(FolderReconcileError::Matter)?;
        let row = ensure_slot(
            pg,
            drive,
            project_id,
            ProjectFolderSlot::Matter(folder),
            Some(parent),
            &parent.drive_folder_id,
            name,
        )
        .await?;
        if let Err(block) = secure_descendant(drive, &row.drive_folder_id, &members, false).await? {
            return Ok(blocked(block));
        }
        rows.insert(row.slot.key(), row);
    }
    // Canonical slots are not the whole provider tree: empty custom folders and file-level
    // grants can independently expose documents. Check both working and intake descendants.
    for (root_id, clients_allowed) in [
        (&root.drive_folder_id, false),
        (&submissions.drive_folder_id, true),
    ] {
        for folder in drive.list_tree_folders(root_id).await? {
            if project_folders::recorded_elsewhere(pg, project_id, &folder.id).await? {
                return Ok(blocked(FolderBlock::ParentMismatch));
            }
            if &folder.id == root_id {
                continue;
            }
            if let Err(block) =
                secure_descendant(drive, &folder.id, &members, clients_allowed).await?
            {
                return Ok(blocked(block));
            }
        }
        for entry in drive.list_tree_items(root_id).await? {
            for ancestor in &entry.ancestor_ids {
                if project_folders::recorded_elsewhere(pg, project_id, ancestor).await? {
                    return Ok(blocked(FolderBlock::ParentMismatch));
                }
            }
            if let Err(block) =
                secure_file(drive, &entry.file.id, &members, clients_allowed).await?
            {
                return Ok(blocked(block));
            }
        }
    }
    // Publish only after every required folder passed identity and access validation.
    for row in rows.values().chain(std::iter::once(&submissions)) {
        project_folders::mark_verified(pg, project_id, row.slot, &row.drive_folder_id).await?;
    }
    crate::projects::set_drive_folder_id(db, project_id, Some(&root.drive_folder_id))
        .await
        .map_err(|_| FolderReconcileError::Matter)?;
    lane.commit().await?;
    Ok(FolderReconcileReport {
        project_id,
        outcome: FolderOutcome::Complete,
        client_submissions_id: Some(submissions.drive_folder_id),
    })
}

/// Revalidate current provider ACLs without changing permissions or reservations.
///
/// # Errors
/// Missing verified slots, provider failures, or any grant outside the current matter policy.
// Preserve the ordered provider audit and its fail-closed publication boundary.
#[allow(clippy::too_many_lines)]
pub async fn verify_for_import<D: DriveService + ?Sized>(
    db: &SurrealDb,
    pg: &PostgresDb,
    drive: &D,
    project_id: Uuid,
) -> Result<(), FolderReconcileError> {
    let trusted = std::env::var("NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER")
        .ok()
        .into_iter()
        .map(|v| email(&v))
        .collect();
    let policy = FolderProvisioningPolicy {
        projects_parent_id: String::new(),
        trusted_organizers: trusted,
    };
    let members = members(db, project_id, &policy).await?;
    if members.firm.is_empty() {
        return Err(FolderReconcileError::PolicyBlocked(
            FolderBlock::MissingFirmParticipant,
        ));
    }
    for (slot, clients_allowed) in [
        (ProjectFolderSlot::Root, false),
        (ProjectFolderSlot::ClientSubmissions, true),
    ] {
        let root = project_folders::get_verified(pg, project_id, slot)
            .await?
            .ok_or(FolderReconcileError::PolicyBlocked(
                FolderBlock::ParentMismatch,
            ))?;
        let root_access = drive.folder_access(&root.drive_folder_id).await?;
        if root.parent_drive_folder_id.as_ref().is_none_or(|parent| {
            root_access.parent_ids.len() != 1 || root_access.parent_ids.first() != Some(parent)
        }) {
            return Err(FolderReconcileError::PolicyBlocked(
                FolderBlock::ParentMismatch,
            ));
        }
        for folder in drive.list_tree_folders(&root.drive_folder_id).await? {
            if project_folders::recorded_elsewhere(pg, project_id, &folder.id).await? {
                return Err(FolderReconcileError::PolicyBlocked(
                    FolderBlock::ParentMismatch,
                ));
            }
            let access = drive.folder_access(&folder.id).await?;
            if folder.id != root.drive_folder_id {
                inspect_descendant(&access, &members, clients_allowed)
                    .map_err(FolderReconcileError::PolicyBlocked)?;
                continue;
            }
            inspect(&access, &members, clients_allowed)
                .map_err(FolderReconcileError::PolicyBlocked)?;
            inspect_content_grants(&access, &members, clients_allowed)
                .map_err(FolderReconcileError::PolicyBlocked)?;
            if !access.inherited_permissions_disabled || !access.can_list_children {
                return Err(FolderReconcileError::PolicyBlocked(
                    FolderBlock::ProviderCapability,
                ));
            }
            if !clients_allowed
                && access.permissions.iter().any(|g| {
                    g.direct
                        && g.email
                            .as_deref()
                            .is_some_and(|e| members.client.contains(&email(e)))
                })
            {
                return Err(FolderReconcileError::PolicyBlocked(
                    FolderBlock::UntrustedPermission,
                ));
            }
        }
        for entry in drive.list_tree_items(&root.drive_folder_id).await? {
            for id in entry.ancestor_ids {
                if project_folders::recorded_elsewhere(pg, project_id, &id).await? {
                    return Err(FolderReconcileError::PolicyBlocked(
                        FolderBlock::ParentMismatch,
                    ));
                }
            }
            let access = DriveFolderAccess {
                inherited_permissions_disabled: true,
                can_disable_inheritance: false,
                can_enable_inheritance: true,
                can_list_children: true,
                parent_ids: Vec::new(),
                permissions: drive.item_permissions(&entry.file.id).await?,
            };
            inspect(&access, &members, clients_allowed)
                .map_err(FolderReconcileError::PolicyBlocked)?;
            inspect_content_grants(&access, &members, clients_allowed)
                .map_err(FolderReconcileError::PolicyBlocked)?;
            if !clients_allowed
                && access.permissions.iter().any(|g| {
                    g.direct
                        && g.email
                            .as_deref()
                            .is_some_and(|e| members.client.contains(&email(e)))
                })
            {
                return Err(FolderReconcileError::PolicyBlocked(
                    FolderBlock::UntrustedPermission,
                ));
            }
        }
    }
    Ok(())
}

/// Reuse the same reconciliation for a bounded boot/nightly roster slice.
///
/// # Errors
/// A roster larger than 100 is refused; individual outcomes remain separate.
pub async fn converge_projects<D: DriveService + ?Sized>(
    db: &SurrealDb,
    pg: &PostgresDb,
    drive: &D,
    ids: &[Uuid],
    policy: &FolderProvisioningPolicy,
) -> Result<Vec<Result<FolderReconcileReport, FolderReconcileError>>, FolderReconcileError> {
    if ids.len() > 100 {
        return Err(FolderReconcileError::Matter);
    }
    let mut outcomes = Vec::with_capacity(ids.len());
    for id in ids {
        outcomes.push(reconcile(db, pg, drive, *id, policy).await);
    }
    Ok(outcomes)
}

/// Reconcile one open Project with configured providers. An unconfigured Drive is skipped.
///
/// # Errors
/// Incomplete provider configuration, unavailable PostgreSQL, or reconciliation failure.
pub async fn reconcile_from_env(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<Option<FolderReconcileReport>, FolderReconcileError> {
    let keys = [
        "NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID",
        "NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER",
        "NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON",
    ];
    if !keys.iter().any(|key| std::env::var_os(key).is_some()) {
        return Ok(None);
    }
    let project = crate::projects::find_by_id(db, project_id)
        .await
        .map_err(|_| FolderReconcileError::Matter)?
        .ok_or(FolderReconcileError::Matter)?;
    if project.status != "open" {
        return Ok(None);
    }
    let config =
        cloud::drive::DriveWorkspaceConfig::from_env(cloud::drive::DriveWorkspace::NeonLaw)?;
    let policy = FolderProvisioningPolicy {
        projects_parent_id: config.projects_drive_id.clone(),
        trusted_organizers: BTreeSet::from([email(&config.delegated_user)]),
    };
    let drive = cloud::drive::GoogleDrive::new(config).await?;
    let pg = PostgresDb::from_env()
        .await
        .map_err(|_| FolderReconcileError::PostgresUnavailable)?;
    reconcile(db, &pg, &drive, project_id, &policy)
        .await
        .map(Some)
}

/// Bounded, content-free result journaled independently for each nightly Project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NightlyFolderOutcome {
    Skipped,
    Verified,
    Blocked,
    Failed,
}

/// Visit every admitted Project in order, without a boot-warmup limit.
/// Provider refusals are journaled outcomes; only journal transport errors stop the pass.
///
/// # Errors
/// Returns the caller's durable journal error without concealing it.
pub async fn visit_nightly_roster<I, F, Fut, E>(
    projects: I,
    mut visit: F,
) -> Result<Vec<NightlyFolderOutcome>, E>
where
    I: IntoIterator<Item = Uuid>,
    F: FnMut(Uuid) -> Fut,
    Fut: std::future::Future<Output = Result<NightlyFolderOutcome, E>>,
{
    let mut outcomes = Vec::new();
    for project_id in projects {
        outcomes.push(visit(project_id).await?);
    }
    Ok(outcomes)
}

/// Reconcile one Project without allowing its failure to starve the nightly roster.
pub async fn nightly_from_env(db: &SurrealDb, project_id: Uuid) -> NightlyFolderOutcome {
    let outcome = match Box::pin(reconcile_from_env(db, project_id)).await {
        Ok(None) => NightlyFolderOutcome::Skipped,
        Ok(Some(report)) if report.outcome == FolderOutcome::Complete => {
            NightlyFolderOutcome::Verified
        }
        Ok(Some(_)) => NightlyFolderOutcome::Blocked,
        Err(_) => NightlyFolderOutcome::Failed,
    };
    tracing::info!(%project_id, ?outcome, "nightly Project Folder reconciliation");
    outcome
}

/// Best-effort bounded startup convergence. The independent nightly full-roster pass retries the rest.
///
/// # Errors
/// Project discovery failure; individual provider failures do not prevent other projects.
pub async fn backfill_from_env(db: &SurrealDb) -> Result<usize, FolderReconcileError> {
    if std::env::var_os("NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID").is_none() {
        return Ok(0);
    }
    let projects = crate::projects::all(db)
        .await
        .map_err(|_| FolderReconcileError::Matter)?;
    let mut verified = 0;
    for project in projects
        .into_iter()
        .filter(|p| p.status == "open")
        .take(100)
    {
        match Box::pin(reconcile_from_env(db, project.id)).await {
            Ok(Some(report)) if report.outcome == FolderOutcome::Complete => verified += 1,
            Ok(None) => break,
            Ok(Some(_)) => {
                tracing::warn!(project_id=%project.id,"folder startup reconciliation blocked");
            }
            Err(_) => tracing::warn!(project_id=%project.id,"folder startup reconciliation failed"),
        }
    }
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloud::drive::{DrivePermissionAccess, FakeDrive};
    fn members() -> Members {
        Members {
            firm: BTreeSet::from(["lawyer@example.com".to_string()]),
            client: BTreeSet::from(["client@example.com".to_string()]),
            trusted: BTreeSet::new(),
        }
    }
    fn member(address: &str) -> DriveMember {
        DriveMember {
            kind: DriveMemberKind::User,
            email: address.to_string(),
        }
    }
    #[tokio::test]
    async fn descendants_inherit_root_without_redundant_direct_grants() {
        let drive = FakeDrive::default();
        let root = drive.create_folder("root").await.unwrap();
        let members = members();
        assert_eq!(
            secure(&drive, &root.id, &members, false, true)
                .await
                .unwrap(),
            Ok(())
        );
        let child = drive
            .ensure_child_folder(&root.id, "Work Product")
            .await
            .unwrap();
        drive.restrict_folder(&child.id).await.unwrap();
        drive
            .set_member_permission(&child.id, &member("client@example.com"), DriveRole::Writer)
            .await
            .unwrap();
        drive
            .set_member_permission(&child.id, &member("lawyer@example.com"), DriveRole::Writer)
            .await
            .unwrap();
        assert_eq!(
            secure_descendant(&drive, &child.id, &members, false)
                .await
                .unwrap(),
            Ok(())
        );
        let access = drive.folder_access(&child.id).await.unwrap();
        assert!(!access.inherited_permissions_disabled);
        assert!(drive.members(&child.id).is_empty());
        assert!(access.permissions.iter().any(|grant| grant.inherited
            && !grant.direct
            && grant.email.as_deref() == Some("lawyer@example.com")));
        drive
            .remove_member_permission(&root.id, &member("lawyer@example.com"))
            .await
            .unwrap();
        assert_eq!(
            inspect_descendant(
                &drive.folder_access(&child.id).await.unwrap(),
                &members,
                false
            ),
            Err(FolderBlock::ProviderCapability)
        );
    }
    #[tokio::test]
    async fn client_root_grant_is_retained_until_safe_intake_is_established() {
        let drive = FakeDrive::default();
        let root = drive.create_folder("root").await.unwrap();
        let intake = drive.create_folder("intake").await.unwrap();
        let members = members();
        drive
            .set_member_permission(&root.id, &member("client@example.com"), DriveRole::Writer)
            .await
            .unwrap();
        assert_eq!(
            secure(&drive, &root.id, &members, false, false)
                .await
                .unwrap(),
            Err(FolderBlock::ProviderCapability)
        );
        assert!(drive
            .members(&root.id)
            .contains(&(member("client@example.com"), DriveRole::Writer)));
        assert_eq!(
            secure(&drive, &intake.id, &members, true, false)
                .await
                .unwrap(),
            Ok(())
        );
        assert_eq!(
            secure(&drive, &root.id, &members, false, true)
                .await
                .unwrap(),
            Ok(())
        );
        assert!(!drive
            .members(&root.id)
            .contains(&(member("client@example.com"), DriveRole::Writer)));
        assert!(drive
            .members(&intake.id)
            .contains(&(member("client@example.com"), DriveRole::Writer)));
        assert_eq!(
            secure(&drive, &root.id, &members, false, true)
                .await
                .unwrap(),
            Ok(())
        );
    }
    #[tokio::test]
    async fn denied_isolation_does_not_claim_success_or_remove_client_grant() {
        let drive = FakeDrive::default();
        let intake = drive.create_folder("intake").await.unwrap();
        let members = members();
        assert!(drive.block_folder_restriction(&intake.id));
        assert_eq!(
            secure(&drive, &intake.id, &members, true, false)
                .await
                .unwrap(),
            Err(FolderBlock::ProviderCapability)
        );
        assert!(
            !drive
                .folder_access(&intake.id)
                .await
                .unwrap()
                .inherited_permissions_disabled
        );
    }
    #[test]
    fn inherited_organizer_and_unknown_direct_grants_never_bypass_firm_policy() {
        let members = members();
        let mut access = DriveFolderAccess {
            inherited_permissions_disabled: true,
            can_disable_inheritance: true,
            can_enable_inheritance: true,
            can_list_children: true,
            parent_ids: Vec::new(),
            permissions: vec![DrivePermissionAccess {
                view: None,
                id: "p".into(),
                permission_type: "user".into(),
                email: Some("outside@example.com".into()),
                role: "organizer".into(),
                inherited: true,
                direct: false,
            }],
        };
        assert_eq!(
            inspect(&access, &members, false),
            Err(FolderBlock::UntrustedOrganizer)
        );
        access.permissions[0].role = "writer".into();
        access.permissions[0].direct = true;
        assert_eq!(
            inspect(&access, &members, false),
            Err(FolderBlock::UntrustedPermission)
        );
        access.permissions[0].permission_type = "domain".into();
        access.permissions[0].email = None;
        assert_eq!(
            inspect(&access, &members, false),
            Err(FolderBlock::UntrustedPermission)
        );
    }
    #[tokio::test]
    async fn direct_file_grants_are_repaired_only_for_known_clients_and_unknowns_block() {
        let drive = FakeDrive::default();
        let root = drive.create_folder("root").await.unwrap();
        let file = drive
            .add_file(
                &root.id,
                "memo.txt",
                "text/plain",
                b"synthetic".to_vec(),
                None,
            )
            .unwrap();
        let members = members();
        drive
            .set_member_permission(&file.id, &member("client@example.com"), DriveRole::Reader)
            .await
            .unwrap();
        assert_eq!(
            secure_file(&drive, &file.id, &members, false)
                .await
                .unwrap(),
            Ok(())
        );
        assert!(drive.item_permissions(&file.id).await.unwrap().is_empty());
        drive
            .set_member_permission(&file.id, &member("unknown@example.com"), DriveRole::Reader)
            .await
            .unwrap();
        assert_eq!(
            secure_file(&drive, &file.id, &members, false)
                .await
                .unwrap(),
            Err(FolderBlock::UntrustedPermission)
        );
        assert_eq!(drive.item_permissions(&file.id).await.unwrap().len(), 1);
    }

    async fn postgres() -> Option<PostgresDb> {
        use sqlx::postgres::{PgConnectOptions, PgSslMode};
        let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
            assert_ne!(std::env::var("NAV_REQUIRE_POSTGRES").as_deref(), Ok("1"));
            return None;
        };
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .database(
                &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                    .unwrap_or_else(|_| "smoke".into()),
            )
            .ssl_mode(PgSslMode::Disable);
        let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
        let name = format!("folder_reconcile_{}", Uuid::now_v7().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
        pg.migrate_embedded(None).await.unwrap();
        Some(pg)
    }
    async fn fixture(pg: &PostgresDb) -> (SurrealDb, Uuid, FakeDrive, FolderProvisioningPolicy) {
        fixture_named(pg, "synthetic-folder").await
    }
    async fn fixture_named(
        pg: &PostgresDb,
        code: &str,
    ) -> (SurrealDb, Uuid, FakeDrive, FolderProvisioningPolicy) {
        use crate::persons::{NewPerson, Role};
        let db = crate::test_support::mem_surreal().await;
        // This fixture crosses into PostgreSQL, so seed the reference rows
        // that the minimal Surreal-only Entity helper intentionally omits.
        db.query("CREATE $jurisdiction SET name = 'Synthetic Source', code = 'ZZ', jurisdiction_type = 'country'; CREATE $kind SET name = 'Synthetic Source';")
            .bind(("jurisdiction", crate::surreal::record_id("jurisdiction", crate::test_support::SEED_ENTITY_JURISDICTION_ID)))
            .bind(("kind", crate::surreal::record_id("entity_type", crate::test_support::SEED_ENTITY_TYPE_ID)))
            .await.unwrap().check().unwrap();
        crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let entity = crate::test_support::seed_entity(&db).await;
        let lawyer = crate::persons::create(
            &db,
            &NewPerson::with_role("Synthetic Lawyer", "lawyer@example.com", Role::Lawyer),
        )
        .await
        .unwrap();
        let client = crate::persons::create(
            &db,
            &NewPerson::with_role("Synthetic Client", "client@example.com", Role::Client),
        )
        .await
        .unwrap();
        let project = crate::projects::open_matter(
            &db,
            &crate::projects::OpenMatterCommand {
                name: "Synthetic Project".into(),
                code: code.into(),
                client_id: client.id,
                entity_id: entity,
                jurisdiction_id: None,
                description: None,
                brand: "neon".into(),
                attestation: true,
                acting_person_id: lawyer.id,
                closed_at: None,
            },
        )
        .await
        .unwrap();
        let jurisdiction = Uuid::now_v7();
        let kind = Uuid::now_v7();
        let pgentity = Uuid::now_v7();
        sqlx::query("INSERT INTO jurisdiction(id,name) VALUES ($1,$2)")
            .bind(jurisdiction)
            .bind(format!("Synthetic {jurisdiction}"))
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO entity_type(id,name) VALUES ($1,$2)")
            .bind(kind)
            .bind(format!("Synthetic {kind}"))
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic',$2,$3)")
            .bind(pgentity).bind(kind).bind(jurisdiction).execute(pg.pool()).await.unwrap();
        sqlx::query("INSERT INTO project(id,code,name,status,entity_id) VALUES ($1,$3,'Synthetic','open',$2)")
            .bind(project.id).bind(pgentity).bind(code).execute(pg.pool()).await.unwrap();
        let drive = FakeDrive::default();
        let parent = drive.create_folder("shared-parent").await.unwrap();
        let policy = FolderProvisioningPolicy {
            projects_parent_id: parent.id,
            trusted_organizers: BTreeSet::new(),
        };
        (db, project.id, drive, policy)
    }
    #[tokio::test]
    async fn legacy_surreal_project_converges_without_preseeded_postgres_project() {
        let Some(pg) = postgres().await else { return };
        let (db, project_id, drive, policy) = fixture(&pg).await;
        sqlx::query("DELETE FROM project WHERE id=$1")
            .bind(project_id)
            .execute(pg.pool())
            .await
            .unwrap();
        let report = reconcile(&db, &pg, &drive, project_id, &policy)
            .await
            .unwrap();
        assert_eq!(report.outcome, FolderOutcome::Complete);
        let projected: Uuid = sqlx::query_scalar("SELECT entity_id FROM project WHERE id=$1")
            .bind(project_id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let source = crate::projects::find_by_id(&db, project_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(projected, source.entity_id);
        assert!(
            project_folders::get_verified(&pg, project_id, ProjectFolderSlot::Root)
                .await
                .unwrap()
                .is_some()
        );
    }
    #[tokio::test]
    async fn reserved_uncreated_root_recovers_same_id_and_all_slots_converge() {
        let Some(pg) = postgres().await else {
            return;
        };
        let (db, id, drive, policy) = fixture(&pg).await;
        let reserved = drive.allocate_folder_id().await.unwrap();
        project_folders::bind(
            &pg,
            id,
            ProjectFolderSlot::Root,
            &reserved,
            None,
            Some(&policy.projects_parent_id),
        )
        .await
        .unwrap();
        assert!(
            project_folders::get_verified(&pg, id, ProjectFolderSlot::Root)
                .await
                .unwrap()
                .is_none()
        );
        let first = reconcile(&db, &pg, &drive, id, &policy).await.unwrap();
        assert_eq!(first.outcome, FolderOutcome::Complete);
        let before = project_folders::list_verified(&pg, id).await.unwrap();
        assert_eq!(before.len(), 16);
        assert_eq!(
            project_folders::get_verified(&pg, id, ProjectFolderSlot::Root)
                .await
                .unwrap()
                .unwrap()
                .drive_folder_id,
            reserved
        );
        let retry = reconcile(&db, &pg, &drive, id, &policy).await.unwrap();
        assert_eq!(retry.outcome, FolderOutcome::Complete);
        let after = project_folders::list_verified(&pg, id).await.unwrap();
        assert_eq!(
            before
                .iter()
                .map(|r| (r.slot.key(), &r.drive_folder_id))
                .collect::<Vec<_>>(),
            after
                .iter()
                .map(|r| (r.slot.key(), &r.drive_folder_id))
                .collect::<Vec<_>>()
        );
    }
    #[tokio::test]
    async fn provider_drift_clears_all_publication_and_recovery_reuses_slots() {
        let Some(pg) = postgres().await else {
            return;
        };
        let (db, id, drive, policy) = fixture(&pg).await;
        assert_eq!(
            reconcile(&db, &pg, &drive, id, &policy)
                .await
                .unwrap()
                .outcome,
            FolderOutcome::Complete
        );
        let root = project_folders::get(&pg, id, ProjectFolderSlot::Root)
            .await
            .unwrap()
            .unwrap();
        assert!(drive.set_folder_access_state(&root.drive_folder_id, false, false));
        assert_eq!(
            reconcile(&db, &pg, &drive, id, &policy)
                .await
                .unwrap()
                .outcome,
            FolderOutcome::Blocked(FolderBlock::ProviderCapability)
        );
        assert!(project_folders::list_verified(&pg, id)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(project_folders::list(&pg, id).await.unwrap().len(), 16);
        assert!(drive.set_folder_access_state(&root.drive_folder_id, false, true));
        assert_eq!(
            reconcile(&db, &pg, &drive, id, &policy)
                .await
                .unwrap()
                .outcome,
            FolderOutcome::Complete
        );
        assert_eq!(
            project_folders::list_verified(&pg, id).await.unwrap().len(),
            16
        );
    }
    #[tokio::test]
    async fn nightly_full_roster_continues_after_real_provider_block_and_database_failure() {
        let Some(pg) = postgres().await else {
            return;
        };
        let blocked = fixture_named(&pg, "blocked-folder").await;
        let mut later = fixture_named(&pg, "later-folder").await;
        // One provider allocates globally unique IDs across the roster. Two
        // independent FakeDrives restart their counters and model ID reuse.
        later.2 = blocked.2.clone();
        let root = blocked
            .2
            .ensure_child_folder(&blocked.3.projects_parent_id, "blocked-folder")
            .await
            .unwrap();
        assert!(blocked.2.set_folder_access_state(&root.id, false, false));
        let missing = Uuid::now_v7();
        let mut roster = vec![missing; 101];
        roster.extend([blocked.1, later.1]);
        let outcomes = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            visit_nightly_roster(roster, |id| {
                let blocked = &blocked;
                let later = &later;
                let pg = &pg;
                async move {
                    let context = if id == later.1 { later } else { blocked };
                    let outcome = match reconcile(&context.0, pg, &context.2, id, &context.3).await
                    {
                        Ok(report) if report.outcome == FolderOutcome::Complete => {
                            NightlyFolderOutcome::Verified
                        }
                        Ok(_) => NightlyFolderOutcome::Blocked,
                        Err(_) => NightlyFolderOutcome::Failed,
                    };
                    Ok::<_, std::convert::Infallible>(outcome)
                }
            }),
        )
        .await
        .expect("bounded roster pass")
        .unwrap();
        assert_eq!(outcomes.len(), 103, "nightly is not capped at boot's 100");
        assert!(outcomes[..101]
            .iter()
            .all(|v| *v == NightlyFolderOutcome::Failed));
        assert_eq!(outcomes[101], NightlyFolderOutcome::Blocked);
        assert_eq!(outcomes[102], NightlyFolderOutcome::Verified);
        assert!(project_folders::list_verified(&pg, blocked.1)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            project_folders::list_verified(&pg, later.1)
                .await
                .unwrap()
                .len(),
            16
        );
    }
}
