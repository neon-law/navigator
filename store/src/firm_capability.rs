//! The typed Firm-capability resolver (ENG-463).
//!
//! Rego admits routes; it cannot query `person_firm_role`, and making it the
//! data-authorization engine would split the source of truth. This module is
//! the one place that decides whether an actor may exercise a given
//! Firm-scoped capability against a given target Firm — the resolver every
//! Firm-scoped read or command routes through instead of re-deriving its own
//! membership check.
//!
//! [`FirmCapability`] is deliberately narrow and closed: a new Firm-scoped
//! command adds its own variant rather than reusing an existing one or
//! falling back to a blanket "is admin" boolean. Owner is system-wide and
//! holds every capability on every Firm without a `person_firm_role` row
//! (`docs/access-model.md`'s Owner-governance carve-out). Integration secrets
//! and Relationship Log observations require Firm membership at the applicable
//! tier. Admin, Lawyer, and
//! Clerk are constrained to the Firms they hold a membership row on, and
//! which capabilities that membership tier admits; Client holds no Firm
//! capability at all.
//!
//! Firm capability does not replace Project participation. Matter documents,
//! notations, and other legal content stay governed by
//! [`crate::access::matter_viewer`] and the Project-side command rules.

use crate::application_store::ApplicationStore;
use crate::firms::{self, FirmError, FirmMembership};
use crate::persons::Role;
use uuid::Uuid;

/// One Firm-scoped capability an actor may exercise against a target Firm.
///
/// [`Self::ViewDirectory`]
/// gates [`crate::firms::visible_person_ids`] and
/// [`crate::projects::matter_directory_for`]; [`Self::ManageMembership`]
/// gates [`crate::firms::add_membership`] and
/// [`crate::firms::ensure_membership`]. [`Self::ManageAdminDri`] gates
/// [`crate::firms::appoint_admin_dri`] (ENG-499): it admits no membership
/// tier at all, so only Owner — who [`resolve`] allows before any membership
/// read — ever holds it. An Admin DRI administers their own Firm's settings
/// and integrations, but appointing or transferring the designation is
/// Owner's alone. [`Self::ManageBrand`] gates a Firm's brand presentation and
/// public logo/font assets; [`Self::ManageBrandContent`] gates its publication
/// catalog and social accounts. Both admit only that Firm's Admin DRI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmCapability {
    /// Read the Firm's scoped people and matter directories.
    ViewDirectory,
    /// Record and read scoped Relationship Log observations as a Firm member.
    AccessRelationshipObservations,
    /// Read and post the Firm-owned accounting journal.
    ManageJournal,
    /// Add or change a person's membership at the Firm.
    ManageMembership,
    /// Remove another administrator’s Firm membership or admin privileges.
    RemoveAdmin,
    /// Appoint or transfer the Firm's Admin DRI designation.
    ManageAdminDri,
    /// Create, replace, or revoke a typed integration secret. Only the
    /// Firm's Admin DRI holds this capability; Owner governs the DRI but is
    /// intentionally not an ordinary secret writer — unless that Owner is
    /// the Firm's own Admin DRI.
    ManageIntegrationSecrets,
    /// Inspect integration-secret metadata without receiving plaintext.
    ViewIntegrationSecretMetadata,
    /// Use the Firm's provider integrations for a Project. Owner may use this
    /// capability system-wide; an Admin must hold a membership row on the
    /// target Firm.
    UseIntegrations,
    /// Read Attorney Calendars through the Firm's Workspace delegation.
    /// Owner may use this system-wide. An Admin or Lawyer membership on the
    /// Firm admits it. A caller with no membership on that Firm cannot spend
    /// the deployment's delegation.
    ReadAttorneyCalendars,
    /// Read or void an envelope in the Firm's own DocuSign account by its
    /// ID, whether or not a Project records it. An Admin or Lawyer membership
    /// on the Firm admits it, as a seat in the Firm's DocuSign account would.
    FirmEnvelopes,
    /// Create or replace a Firm's presentation and public brand assets. Only
    /// the Firm's Admin DRI holds this capability.
    ManageBrand,
    /// Manage the Firm's published Brand content and social accounts. Only
    /// the Firm's Admin DRI holds this capability.
    ManageBrandContent,
    /// Approve or revoke verified senders for Firm-scoped message summaries.
    ManageSummaryApprovals,
    /// Request, verify, and restore the Firm's Project Recovery Archives, and
    /// authorize their governed retention purge. Owner holds it system-wide;
    /// an Admin needs an Admin membership row on the target Firm.
    ManageRecoveryArchives,
    /// Move one of the deployment's Brands to a different Firm. No
    /// membership tier admits it: a move re-points whose legal entity the
    /// moved brand's public copyright names, which is an ownership decision
    /// only Owner makes.
    MoveBrand,
    /// Attach one of the deployment's closed house brands to a Firm. No
    /// membership tier admits it — pointing the seat at a firm is the same
    /// ownership decision moving it is.
    AttachBrand,
    /// Read a Firm's Brand call/SMS counts, billed tenths and receiving-number metadata.
    ViewCommunications,
    /// Read a Firm's plan-work measurement: matter counts per plan and state,
    /// SLA misses, minutes by stage, and matter margins. The economics are
    /// Firm-sensitive, so like [`Self::ViewCommunications`] only the Firm's
    /// Admin membership admits it.
    ViewPlanMetrics,
    /// Read, set, and walk the history of the Firm's assumptions (ENG-1264):
    /// the stated premises the CEO digest checks and kits resolve at run
    /// time. The values are the Firm's own economics, so like
    /// [`Self::ViewPlanMetrics`] only the Firm's Admin membership admits it.
    ManageFirmAssumptions,
    /// Add a Legal Plan to the Firm's vocabulary (its portal, contact, and
    /// service levels). The Firm's panel relationships are Admin business,
    /// like [`Self::ManageFirmAssumptions`].
    ManageLegalPlans,
}

impl FirmCapability {
    /// Whether a `person_firm_role` row carrying this membership tier admits
    /// this capability. `ViewDirectory` is open to every firm tier; only
    /// `Admin` membership may manage who else belongs to the Firm.
    /// `ManageAdminDri`, `MoveBrand`, and `AttachBrand` admit no membership
    /// tier — Owner is the only actor [`resolve`] ever grants them to.
    #[must_use]
    fn admits(self, membership: FirmMembership) -> bool {
        match self {
            Self::ViewDirectory => true,
            Self::AccessRelationshipObservations
            | Self::ReadAttorneyCalendars
            | Self::FirmEnvelopes => {
                matches!(membership, FirmMembership::Admin | FirmMembership::Lawyer)
            }
            Self::ManageJournal
            | Self::ManageMembership
            | Self::ManageIntegrationSecrets
            | Self::ViewIntegrationSecretMetadata
            | Self::UseIntegrations
            | Self::ManageRecoveryArchives
            | Self::ViewCommunications
            | Self::ViewPlanMetrics
            | Self::ManageFirmAssumptions
            | Self::ManageLegalPlans
            | Self::ManageBrand => membership == FirmMembership::Admin,
            Self::RemoveAdmin
            | Self::ManageBrandContent
            | Self::ManageSummaryApprovals
            | Self::ManageAdminDri
            | Self::MoveBrand
            | Self::AttachBrand => false,
        }
    }

    /// A stable, mechanism-only name for telemetry (ENG-464) — never a
    /// `Debug` derive, so a variant rename does not silently change what a
    /// dashboard or alert already matches on.
    #[must_use]
    fn telemetry_name(self) -> &'static str {
        match self {
            Self::ViewDirectory => "view_directory",
            Self::AccessRelationshipObservations => "access_relationship_observations",
            Self::ManageJournal => "manage_journal",
            Self::ManageMembership => "manage_membership",
            Self::RemoveAdmin => "remove_admin",
            Self::ManageAdminDri => "manage_admin_dri",
            Self::ManageIntegrationSecrets => "manage_integration_secrets",
            Self::ViewIntegrationSecretMetadata => "view_integration_secret_metadata",
            Self::UseIntegrations => "use_integrations",
            Self::ReadAttorneyCalendars => "read_attorney_calendars",
            Self::FirmEnvelopes => "firm_envelopes",
            Self::ManageBrand => "manage_brand",
            Self::ManageBrandContent => "manage_brand_content",
            Self::ManageSummaryApprovals => "manage_summary_approvals",
            Self::ManageRecoveryArchives => "manage_recovery_archives",
            Self::MoveBrand => "move_brand",
            Self::AttachBrand => "attach_brand",
            Self::ViewCommunications => "view_communications",
            Self::ViewPlanMetrics => "view_plan_metrics",
            Self::ManageFirmAssumptions => "manage_firm_assumptions",
            Self::ManageLegalPlans => "manage_legal_plans",
        }
    }

    fn requires_admin_dri(self) -> bool {
        matches!(
            self,
            Self::ManageIntegrationSecrets
                | Self::RemoveAdmin
                | Self::ViewIntegrationSecretMetadata
                | Self::ManageBrand
                | Self::ManageBrandContent
                | Self::ManageSummaryApprovals
        )
    }

    /// Resolve this capability against one Firm.
    pub async fn resolve(
        self,
        application_store: &ApplicationStore,
        actor_role: Role,
        actor_person_id: Option<Uuid>,
        target_firm_id: Uuid,
    ) -> Result<FirmCapabilityDecision, FirmError> {
        resolve(
            application_store,
            actor_role,
            actor_person_id,
            target_firm_id,
            self,
        )
        .await
    }
}

/// The resolver's answer for one `(actor, target Firm, capability)` question.
///
/// [`Self::FirmNotFound`] is distinct from [`Self::Forbidden`] so a caller can
/// choose to render them identically at the response boundary — the
/// resolver hands back the fact, not the disclosure decision, but leaves it
/// possible to keep the two indistinguishable to the caller across a Firm
/// boundary where that matters (`docs/access-model.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmCapabilityDecision {
    Allowed,
    Forbidden,
    FirmNotFound,
}

impl FirmCapabilityDecision {
    #[must_use]
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// Resolve whether `actor` may exercise `capability` against `target_firm_id`.
///
/// Side-effect-free: reads only, never writes. Owner holds deployment-wide
/// capabilities without a membership row. Client is refused
/// every capability. Admin, Lawyer, and Clerk need a `person_firm_role` row
/// on `target_firm_id` whose membership tier [`FirmCapability::admits`]s the
/// requested capability.
///
/// Emits exactly one `firm_capability.resolve` telemetry event (ENG-464)
/// naming the capability, `target_firm_id`, `actor_person_id` (when known),
/// the outcome, and a stable reason code — the one place every Firm-scoped
/// allow/deny decision becomes auditable, since every caller routes through
/// here rather than re-deriving its own check. Never logs an email or name:
/// an id is a queryable fact, a person's identity is client content.
pub async fn resolve(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    target_firm_id: Uuid,
    capability: FirmCapability,
) -> Result<FirmCapabilityDecision, FirmError> {
    let (decision, reason) = resolve_inner(
        application_store,
        actor_role,
        actor_person_id,
        target_firm_id,
        capability,
    )
    .await?;
    tracing::info!(
        target: "firm_capability.resolve",
        capability = capability.telemetry_name(),
        firm_id = %target_firm_id,
        person_id = actor_person_id.map(|id| id.to_string()),
        allowed = decision.is_allowed(),
        reason,
        "firm capability decision",
    );
    Ok(decision)
}

async fn resolve_inner(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    target_firm_id: Uuid,
    capability: FirmCapability,
) -> Result<(FirmCapabilityDecision, &'static str), FirmError> {
    if firms::find_by_id(application_store, target_firm_id)
        .await?
        .is_none()
    {
        return Ok((FirmCapabilityDecision::FirmNotFound, "firm_not_found"));
    }
    if actor_role == Role::Owner && capability != FirmCapability::AccessRelationshipObservations {
        if !matches!(
            capability,
            FirmCapability::ManageIntegrationSecrets | FirmCapability::RemoveAdmin
        ) {
            return Ok((FirmCapabilityDecision::Allowed, "owner_bypass"));
        }
        let is_dri = match actor_person_id {
            Some(person_id) => {
                firms::membership_for_person(application_store, person_id, target_firm_id)
                    .await?
                    .is_some_and(|row| row.membership == FirmMembership::Admin && row.is_dri)
            }
            None => false,
        };
        return Ok(if is_dri {
            (FirmCapabilityDecision::Allowed, "owner_is_admin_dri")
        } else if capability == FirmCapability::RemoveAdmin {
            (FirmCapabilityDecision::Forbidden, "owner_not_admin_dri")
        } else {
            (FirmCapabilityDecision::Forbidden, "owner_not_secret_writer")
        });
    }
    if capability == FirmCapability::AccessRelationshipObservations && !actor_role.is_lawyer_tier()
    {
        return Ok((
            FirmCapabilityDecision::Forbidden,
            "relationship_observations_require_lawyer",
        ));
    }
    if capability == FirmCapability::ViewCommunications && actor_role != Role::Admin {
        return Ok((
            FirmCapabilityDecision::Forbidden,
            "communications_requires_admin",
        ));
    }
    if capability == FirmCapability::ViewPlanMetrics && actor_role != Role::Admin {
        return Ok((
            FirmCapabilityDecision::Forbidden,
            "plan_metrics_requires_admin",
        ));
    }
    if capability == FirmCapability::ManageFirmAssumptions && actor_role != Role::Admin {
        return Ok((
            FirmCapabilityDecision::Forbidden,
            "assumptions_requires_admin",
        ));
    }
    if capability == FirmCapability::RemoveAdmin && !actor_role.is_admin_tier() {
        return Ok((
            FirmCapabilityDecision::Forbidden,
            "remove_admin_requires_admin",
        ));
    }
    if actor_role == Role::Client {
        return Ok((
            FirmCapabilityDecision::Forbidden,
            "client_has_no_firm_capability",
        ));
    }
    let Some(person_id) = actor_person_id else {
        return Ok((FirmCapabilityDecision::Forbidden, "no_person_id"));
    };
    let member = firms::membership_for_person(application_store, person_id, target_firm_id).await?;
    Ok(match member {
        Some(row) if capability.requires_admin_dri() => {
            if row.membership == FirmMembership::Admin && row.is_dri {
                (
                    FirmCapabilityDecision::Allowed,
                    "admin_dri_membership_admits",
                )
            } else {
                (FirmCapabilityDecision::Forbidden, "not_admin_dri")
            }
        }
        Some(row) if capability.admits(row.membership) => {
            (FirmCapabilityDecision::Allowed, "membership_admits")
        }
        Some(_) => (FirmCapabilityDecision::Forbidden, "membership_denies"),
        None => (FirmCapabilityDecision::Forbidden, "not_a_member"),
    })
}

/// Every Firm id where `actor` holds `capability`.
///
/// The batch counterpart to [`resolve`], for a directory listing that scopes
/// itself to every Firm the caller may act on rather than asking about one.
/// Owner holds deployment-wide capabilities on every existing Firm. Integration
/// secrets, admin removal, and Relationship Log observations require Firm membership.
pub async fn allowed_firm_ids(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    capability: FirmCapability,
) -> Result<Vec<Uuid>, FirmError> {
    if actor_role == Role::Owner
        && !matches!(
            capability,
            FirmCapability::ManageIntegrationSecrets
                | FirmCapability::RemoveAdmin
                | FirmCapability::AccessRelationshipObservations
        )
    {
        return Ok(firms::all(application_store)
            .await?
            .into_iter()
            .map(|f| f.id)
            .collect());
    }
    if capability == FirmCapability::AccessRelationshipObservations && !actor_role.is_lawyer_tier()
    {
        return Ok(Vec::new());
    }
    if capability == FirmCapability::ViewCommunications && actor_role != Role::Admin {
        return Ok(Vec::new());
    }
    if capability == FirmCapability::ManageFirmAssumptions && actor_role != Role::Admin {
        return Ok(Vec::new());
    }
    if capability == FirmCapability::RemoveAdmin && !actor_role.is_admin_tier() {
        return Ok(Vec::new());
    }
    if actor_role == Role::Client {
        return Ok(Vec::new());
    }
    let Some(person_id) = actor_person_id else {
        return Ok(Vec::new());
    };
    Ok(firms::memberships_for_person(application_store, person_id)
        .await?
        .into_iter()
        .filter(|row| {
            if capability.requires_admin_dri() {
                row.membership == FirmMembership::Admin && row.is_dri
            } else {
                capability.admits(row.membership)
            }
        })
        .map(|row| row.firm_id)
        .collect())
}

#[cfg(test)]
mod tests {}
