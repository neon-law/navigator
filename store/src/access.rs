//! Row-level visibility helpers.
//!
//! `Role decides the tier; participation decides the scope.` This
//! module is the one place where that mapping turns into SQL:
//! [`visible_projects`] returns the slice of `projects` a given
//! [`Role`] + `person_id` should see, and every project-list / detail
//! handler funnels through it.
//!
//! See [`docs/access-model.md`](../../../docs/access-model.md).

use crate::application_store::ApplicationStore;
use crate::persons::{self, Person, PersonError, Role};
use crate::projects::{self, Project};
use uuid::Uuid;

/// A supervisor lookup that failed in the other engine, reported as the
/// `String` every caller of this module already handles.
fn supervisor_lookup_failed(err: &PersonError) -> String {
    format!("resolve the supervising lawyer: {err}")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectLens {
    Client,
    Lawyer,
}

impl ProjectLens {
    /// Which side of a matter a role-only surface renders from.
    ///
    /// This is a *render* decision — which fields a page emits, which
    /// `asset.visibility` a download resolves, whether a conversation message
    /// is inbound or outbound. It is never the access decision: that is
    /// [`can_see_project`], which dispatches on the same role and reads the
    /// participation ledger. Matter document reads must use [`matter_lens`],
    /// which preserves a client-side participation row for mixed-role people.
    /// A caller that reaches a render site has already passed the gate.
    ///
    /// `Clerk` maps to the client lens so an unforeseen render path fails
    /// closed; the Clerk gate denies the matter surface outright.
    #[must_use]
    pub fn for_role(role: Role) -> Self {
        match role {
            Role::Owner | Role::Admin | Role::Lawyer => Self::Lawyer,
            Role::Clerk | Role::Client => Self::Client,
        }
    }
}

/// Who the caller is *to this matter* — the five distinct renderings the one
/// matter page serves.
///
/// Deliberately separate from [`ProjectLens`], which stays two-valued and keeps
/// answering the only question asset visibility has ever asked: firm side or
/// client side. Folding the two together would make every `asset.visibility`
/// match arm grow from two cases to five, and would mean a variant added for a
/// UI reason silently became a confidentiality decision.
///
/// The order below is the authority order on a matter, not the tier order:
/// a Clerk outranks nobody, and a DRI is the accountable one on their side.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatterViewer {
    /// A client-side participant.
    Client,
    /// The client-side participant carrying the accountability marker. Sees the
    /// client page plus the controls only they may fire, such as plan approval.
    ClientDri,
    /// A supervised non-lawyer. Sees the matter's name, status, and supervising
    /// lawyer — never documents, legal work, or any write.
    Clerk,
    /// A firm-side participant.
    Lawyer,
    /// The firm-side participant carrying the accountability marker. Sees the
    /// workbench plus the matter-level accountability actions.
    LawyerDri,
}

impl MatterViewer {
    /// Which side of the matter this viewer is on, for asset visibility.
    #[must_use]
    pub fn lens(self) -> ProjectLens {
        match self {
            Self::Lawyer | Self::LawyerDri => ProjectLens::Lawyer,
            // A Clerk never reaches an asset at all, but if a render path ever
            // asks, the answer must be the narrower one.
            Self::Client | Self::ClientDri | Self::Clerk => ProjectLens::Client,
        }
    }

    /// `true` for the accountable participant on either side.
    #[must_use]
    pub fn is_dri(self) -> bool {
        matches!(self, Self::ClientDri | Self::LawyerDri)
    }

    /// `true` for the firm side of the matter.
    #[must_use]
    pub fn is_firm_side(self) -> bool {
        matches!(self, Self::Lawyer | Self::LawyerDri)
    }
}

/// Project access through the Brand resolved by the serving host.
#[derive(Clone, Copy)]
pub struct ClientPortal<'a> {
    pub brand: &'a str,
}

impl ClientPortal<'_> {
    /// The existing role-specific list, narrowed to the host for clients.
    pub async fn visible_projects(
        self,
        db: &ApplicationStore,
        person: Option<Uuid>,
        role: Role,
    ) -> Result<Vec<Project>, String> {
        let mut projects = visible_projects(db, person, role).await?;
        if role == Role::Client {
            projects.retain(|project| self.contains(project));
        }
        Ok(projects)
    }

    #[must_use]
    pub fn contains(self, project: &Project) -> bool {
        project.brand == self.brand
    }

    pub async fn matter_viewer(
        self,
        db: &ApplicationStore,
        person: Option<Uuid>,
        role: Role,
        project_id: Uuid,
    ) -> Result<Option<MatterViewer>, String> {
        let viewer = matter_viewer(db, person, role, project_id).await?;
        if role != Role::Client || viewer.is_none() {
            return Ok(viewer);
        }
        let project = projects::find_by_id(db, project_id)
            .await
            .map_err(|error| project_lookup_failed(&error))?;
        Ok(viewer.filter(|_| project.is_some_and(|row| self.contains(&row))))
    }

    pub async fn can_see_project_as_client(
        self,
        db: &ApplicationStore,
        person: Option<Uuid>,
        role: Role,
        project_id: Uuid,
    ) -> Result<bool, String> {
        if role != Role::Client {
            return can_see_project_as_client(db, person, project_id).await;
        }
        Ok(self
            .matter_viewer(db, person, Role::Client, project_id)
            .await?
            .is_some())
    }
}

/// Resolve who this caller is to this matter, or `None` if they may not see it
/// at all.
///
/// This is the matter surface's single entry point: the gate and the render
/// variant are the same question asked once, so a page cannot render a variant
/// its caller was never admitted to. [`can_see_project`] is this function
/// reduced to a boolean.
///
/// Every tier is scoped by the participation ledger, Owner and Admin included
/// (ENG-81) — there is no privileged short-circuit here.
pub async fn matter_viewer(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
    project_id: Uuid,
) -> Result<Option<MatterViewer>, String> {
    let Some(pid) = person_id else {
        return Ok(None);
    };

    // A Clerk is not a narrower Lawyer: the matter must also name at least one
    // currently licensed lawyer among its lawyer DRIs, a different query.
    if role == Role::Clerk {
        return Ok(
            can_see_project_as_clerk(application_store, person_id, role, project_id)
                .await?
                .then_some(MatterViewer::Clerk),
        );
    }

    let Some(row) = projects::participation_for_person(application_store, pid, project_id)
        .await
        .map_err(|error| project_lookup_failed(&error))?
    else {
        return Ok(None);
    };

    if is_client_participation(&row) {
        // A client-side row is the client lens whatever the tier: a lawyer who
        // is also a client on their own matter reads it as a client.
        return Ok(Some(if row.is_client_dri {
            MatterViewer::ClientDri
        } else {
            MatterViewer::Client
        }));
    }

    // Firm-side rows only reach the workbench from a firm tier. A `client`-tier
    // person holding one is a data error, not a promotion.
    if !role.is_lawyer_tier() {
        return Ok(None);
    }
    Ok(Some(if row.is_lawyer_dri {
        MatterViewer::LawyerDri
    } else {
        MatterViewer::Lawyer
    }))
}

/// Resolve the asset-visibility lens for a caller on a matter.
///
/// Matter document reads must use this rather than [`ProjectLens::for_role`]
/// because a client-side participation row wins over the person's system tier.
/// In particular, a lawyer who is also a client on a matter reads that matter
/// through the client lens. `None` means the caller may not see the matter.
pub async fn matter_lens(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
    project_id: Uuid,
) -> Result<Option<ProjectLens>, String> {
    Ok(
        matter_viewer(application_store, person_id, role, project_id)
            .await?
            .map(MatterViewer::lens),
    )
}

/// All projects this person is allowed to see.
///
/// - [`Role::Owner`] / [`Role::Admin`] / [`Role::Lawyer`] → only projects with a
///   firm-side `person_project_roles` row for `person_id`. There is no
///   privileged bypass on the matter surface: a lawyer, an admin, and an owner
///   who have not been put on a matter all do not see it.
/// - [`Role::Client`] → only projects with a client-side row. A firm-side
///   assignment does not put the matter in a client's list.
/// - [`Role::Clerk`] → the supervised set only: a firm-side row *and* a matter
///   one of whose flagged lawyer DRIs currently holds the lawyer tier. Narrower
///   than the Lawyer predicate, so a Clerk is never folded into the lawyer lens.
///
/// `person_id` is `Option` because some test paths build sessions
/// without a linked persons row. When it's `None` every tier sees nothing —
/// fail-closed, with no privileged exception.
pub async fn visible_projects(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
) -> Result<Vec<Project>, String> {
    match role {
        Role::Client => visible_projects_as_client(application_store, person_id).await,
        // A Clerk now reaches the one matter list like everyone else; the
        // dedicated `/clerk` surface is retired. What they see through it is
        // still the supervised set, which is a different query.
        Role::Clerk => visible_projects_as_clerk(application_store, person_id, role).await,
        Role::Owner | Role::Admin | Role::Lawyer => {
            visible_projects_as_lawyer(application_store, person_id, role).await
        }
    }
}

/// All projects visible through the web app's client lens.
///
/// This is intentionally narrower than "has any participation row":
/// firm-side participation such as `attorney` or `paralegal` belongs to
/// `/app/lawyer`, not the client lens.
pub async fn visible_projects_as_client(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
) -> Result<Vec<Project>, String> {
    let Some(pid) = person_id else {
        return Ok(Vec::new());
    };
    visible_projects_matching(application_store, pid, ProjectLens::Client, None).await
}

/// All projects visible through the lawyer workbench lens.
///
/// Every firm tier — Owner and Admin included — sees only matters where they
/// hold a firm-side participation row. There is no silent project-scoping
/// bypass on the matter surface; privileged reach is a place you navigate to.
/// The accountable lawyer always has a row, because designating a lawyer DRI
/// flags their membership row.
pub async fn visible_projects_as_lawyer(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
) -> Result<Vec<Project>, String> {
    if !role.is_lawyer_tier() {
        return Ok(Vec::new());
    }
    let Some(pid) = person_id else {
        return Ok(Vec::new());
    };
    visible_projects_matching(application_store, pid, ProjectLens::Lawyer, None).await
}

/// Participating Projects billed on the current host's Brand and Firm.
pub async fn visible_lawyer_projects_for_brand(
    db: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
    host_brand: &str,
) -> Result<Vec<Project>, String> {
    let mut projects = visible_projects_as_lawyer(db, person_id, role).await?;
    let firm = crate::firms::firm_id_for_brand_key(db, host_brand)
        .await
        .map_err(|error| error.to_string())?;
    projects.retain(|project| {
        project.brand == host_brand && (project.firm_id.is_none() || project.firm_id == firm)
    });
    Ok(projects)
}

/// All projects visible through the supervised Clerk lens.
///
/// Clerk is a non-lawyer role, so its route does not inherit either the
/// client or Lawyer predicates. A project appears only when the Clerk
/// has a firm-side participation row *and* the matter's flagged lawyer-DRI
/// row belongs to a currently licensed lawyer (`lawyer` or `admin`).
///
/// The `/clerk` handlers render a deliberately limited, read-only view.
/// Upload, document-content, drafting, review, approval, Git, MCP, and
/// conversation capabilities each require their own route-level grant.
pub async fn visible_projects_as_clerk(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
) -> Result<Vec<Project>, String> {
    if role != Role::Clerk {
        return Ok(Vec::new());
    }
    let Some(pid) = person_id else {
        return Ok(Vec::new());
    };

    Ok(supervised_projects(application_store, pid)
        .await?
        .into_iter()
        .map(|(project, _supervisor)| project)
        .collect())
}

/// The supervised matters a Clerk may see, each paired with the licensed
/// lawyer accountable for it.
///
/// The licensure check and the supervisor lookup are the same query now that
/// the lawyer DRI is a flagged membership row: a matter qualifies only when one
/// of its flagged rows belongs to a currently licensed `owner`, `admin`, or
/// `lawyer` person. Returning the supervisor alongside the project is what lets
/// `webapp::clerk` render "Supervising lawyer" without a second batch load — and
/// makes it impossible to show a Clerk a matter whose supervisor could not be
/// resolved.
///
/// A matter's lawyer DRIs are a set, and supervision reads it as **any**: one
/// licensed lawyer accountable for the matter is what the Clerk needs, so a
/// second DRI who has since lost the tier does not withdraw the matter. The
/// named supervisor is the alphabetically first qualifying one, which keeps the
/// column stable across reads rather than following row order.
pub async fn supervised_projects(
    application_store: &ApplicationStore,
    person_id: Uuid,
) -> Result<Vec<(Project, Person)>, String> {
    let projects =
        visible_projects_matching(application_store, person_id, ProjectLens::Lawyer, None).await?;
    let mut dri_ids_by_project: std::collections::HashMap<Uuid, Vec<Uuid>> =
        std::collections::HashMap::new();
    for project in &projects {
        let ids: Vec<Uuid> = projects::participations_for_project(application_store, project.id)
            .await
            .map_err(|error| project_lookup_failed(&error))?
            .into_iter()
            .filter(|row| row.is_lawyer_dri)
            .map(|row| row.person_id)
            .collect();
        if !ids.is_empty() {
            dri_ids_by_project.insert(project.id, ids);
        }
    }
    // A Clerk may never be supervised by a non-lawyer, so the DRI has to be a
    // currently licensed `owner`, `admin`, or `lawyer` person. The tier is
    // filtered in Rust because the rows come from the other engine: the DRI
    // designation is a `person_project_role` row and the `role` it
    // has to satisfy is a SurrealDB `person` field.
    let dri_ids: Vec<Uuid> = dri_ids_by_project.values().flatten().copied().collect();
    let licensed: std::collections::HashMap<Uuid, Person> =
        persons::find_by_ids(application_store, &dri_ids)
            .await
            .map_err(|err| supervisor_lookup_failed(&err))?
            .into_iter()
            .filter(|supervisor| supervisor.role.is_lawyer_tier())
            .map(|supervisor| (supervisor.id, supervisor))
            .collect();
    Ok(projects
        .into_iter()
        .filter_map(|project| {
            let supervisor = dri_ids_by_project
                .get(&project.id)?
                .iter()
                .filter_map(|id| licensed.get(id))
                .min_by(|a, b| a.name.cmp(&b.name))?
                .clone();
            Some((project, supervisor))
        })
        .collect())
}

/// `true` iff the caller may see the given project. Single-row
/// counterpart to [`visible_projects`] — same semantics, one
/// `SELECT 1` instead of loading every membership row.
///
/// Project-detail handlers call this *before* fetching the project
/// itself so an unauthorised caller never even pulls the row into
/// the response.
/// This is **the matter surface's gate** — `/app/projects/{id}` and its
/// children — and it is the one place with no privileged short-circuit: a
/// participation row is required of every tier, Owner and Admin included
/// (ENG-81, decided 2026-08-05).
///
/// It deliberately does *not* delegate the firm arm to
/// [`can_see_project_as_lawyer`], which still carries the documented
/// project-scoping bypass for the surfaces this slice did not move (most of
/// `/app/api/*`, the notation walker, contract reviews). Those are ENG-83's to
/// collapse. Until then the two functions differ for exactly one input —
/// an Owner or Admin with no row — so reach for this one on the matter
/// surface and that one everywhere else.
///
/// A door added since ENG-81 that returns matter *content* starts here rather
/// than waiting for ENG-83: the private notation inventory and a notation's
/// filed answers already do.
pub async fn can_see_project(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
    project_id: Uuid,
) -> Result<bool, String> {
    Ok(
        matter_viewer(application_store, person_id, role, project_id)
            .await?
            .is_some(),
    )
}

/// `true` iff the caller may see the project through the client lens.
pub async fn can_see_project_as_client(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    project_id: Uuid,
) -> Result<bool, String> {
    let Some(pid) = person_id else {
        return Ok(false);
    };
    projects::participation_for_person(application_store, pid, project_id)
        .await
        .map_err(|error| project_lookup_failed(&error))
        .map(|row| row.is_some_and(|row| is_client_participation(&row)))
}

/// Legacy firm-side command access, including the Owner and the Project's
/// Firm Admin. Matter content uses [`can_see_project`]'s participation gate.
pub async fn can_see_project_as_lawyer(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
    project_id: Uuid,
) -> Result<bool, String> {
    projects::can_access_as_lawyer(application_store, person_id, role, project_id)
        .await
        .map_err(|error| project_lookup_failed(&error))
}

/// `true` iff a supervised Clerk may see the project through `/clerk`.
pub async fn can_see_project_as_clerk(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
    project_id: Uuid,
) -> Result<bool, String> {
    Ok(
        visible_projects_as_clerk(application_store, person_id, role)
            .await?
            .iter()
            .any(|project| project.id == project_id),
    )
}

/// `true` iff `viewer_id` (holding `viewer_role`) may see `target_id`'s
/// avatar.
///
/// A viewer sees their own avatar, Owner/Admin sees any avatar, and every
/// other authenticated viewer must share at least one Project participation
/// row with the target. A denial is intentionally indistinguishable from a
/// missing avatar at the route layer.
pub async fn avatar_visible_to(
    application_store: &ApplicationStore,
    viewer_id: Uuid,
    viewer_role: Role,
    target_id: Uuid,
) -> Result<bool, String> {
    if viewer_id == target_id {
        return Ok(true);
    }
    if matches!(viewer_role, Role::Owner | Role::Admin) {
        return Ok(true);
    }

    let viewer_projects: std::collections::HashSet<Uuid> =
        projects::participations_for_person(application_store, viewer_id)
            .await
            .map_err(|error| project_lookup_failed(&error))?
            .into_iter()
            .map(|row| row.project_id)
            .collect();
    if viewer_projects.is_empty() {
        return Ok(false);
    }
    Ok(
        projects::participations_for_person(application_store, target_id)
            .await
            .map_err(|error| project_lookup_failed(&error))?
            .into_iter()
            .any(|row| viewer_projects.contains(&row.project_id)),
    )
}

fn project_lookup_failed(error: &projects::ProjectStoreError) -> String {
    format!("projects cluster: {error}")
}

fn is_client_participation(row: &projects::PersonProjectRole) -> bool {
    projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str()) || row.is_client_dri
}

pub(crate) fn is_firm_participation(row: &projects::PersonProjectRole) -> bool {
    !projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str()) && !row.is_client_dri
}

/// Resolve one exact Project code within the person's firm-side memberships.
pub async fn visible_firm_project_by_code(
    application_store: &ApplicationStore,
    person_id: Uuid,
    code: &str,
) -> Result<Option<Project>, String> {
    Ok(visible_projects_matching(
        application_store,
        person_id,
        ProjectLens::Lawyer,
        Some(code),
    )
    .await?
    .pop())
}

async fn visible_projects_matching(
    _application_store: &ApplicationStore,
    person_id: Uuid,
    lens: ProjectLens,
    code: Option<&str>,
) -> Result<Vec<Project>, String> {
    let pg = crate::postgres::tenancy::db()
        .await
        .map_err(|err| project_lookup_failed(&err.into()))?;
    crate::postgres::projects::participating_projects_on(&pg, person_id, lens, code)
        .await
        .map_err(|err| project_lookup_failed(&err.into()))
}

#[cfg(test)]
mod tests {

    /// The access predicates never consult the reference relation. The
    /// token is split so this guard does not match itself.
    #[test]
    fn the_access_predicates_never_read_project_references() {
        let relation = concat!("project_", "reference");
        for (file, source) in [
            ("access.rs", include_str!("access.rs")),
            ("projects.rs", include_str!("projects.rs")),
        ] {
            // The predicates are what matter; a test module may name the
            // relation to prove it is ignored.
            let predicates = source.split("#[cfg(test)]").next().unwrap_or_default();
            assert!(
                !predicates.contains(relation),
                "{file} must not read the {relation} relation: a reference grants no lens"
            );
        }
    }
}
