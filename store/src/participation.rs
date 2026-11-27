//! Participation commands for the SurrealDB projects cluster.
//!
//! `participation` controls project visibility, so every mutation validates its
//! native person/project links and preserves the lawyer-DRI invariant at this
//! shared command boundary: a matter's accountable lawyers are a set, and that
//! set is never empty.
//!
//! Accountability is also the one thing here that names its actor. Every door
//! that moves a marker takes a [`DriActor`], which answers two questions at
//! once — whether this caller may change that side, and whose name goes on the
//! audit row. They are the same field because a rule enforced against one person
//! and recorded against another is not an audit trail.
//!
//! No caller names a participation. Which side of a matter someone is on
//! follows from `persons.role`, so these commands derive it through
//! [`projects::participation_for_role`] and the three doors above them — the
//! lawyer form, `POST /app/api/projects/{id}/participants`, and
//! `link_person_project` — all take a person and nothing else. Deriving it keeps
//! the word from disagreeing with the tier: a `client` recorded as `attorney` is
//! a firm-side row, the matter's own client reading `/app/lawyer`.

use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::persons::Person;
use crate::projects::{self, DriSide, PersonProjectRole};

/// What a write door asks of the matter's accountability markers.
///
/// Participation says who reaches a matter; this says who is *accountable* for
/// it. Every door that does not offer the control leaves it [`Unchanged`], so
/// adding a person never quietly moves accountability.
///
/// Each side is a **set**. A matter can have as many accountable lawyers, and as
/// many accountable client contacts, as the firm has put on it, so
/// [`Designate`] adds to a side and displaces nobody, and [`Clear`] is the only
/// way out of one.
///
/// [`Unchanged`]: DriRequest::Unchanged
/// [`Designate`]: DriRequest::Designate
/// [`Clear`]: DriRequest::Clear
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DriRequest {
    /// Leave both markers exactly as they are.
    #[default]
    Unchanged,
    /// Take this row's person out of whichever side they are accountable on.
    /// Refused for the last lawyer DRI: the lawyer set is never empty.
    Clear,
    /// Add this row's person to the given side's set.
    Designate(DriSide),
}

/// Who is asking for a DRI change.
///
/// Accountability is the one thing on a matter that has to name the person who
/// moved it, so the commands carry the actor rather than leaving the handler to
/// check one thing and the audit trail to record another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriActor {
    /// A trusted internal caller with no person behind it — matter open, the
    /// seed, a workflow step. Not gated, and audited with no actor.
    System,
    /// A signed-in person, gated by [`authorize`] and named in the audit row.
    Person(Uuid),
}

/// Refusals shared by both write doors when a DRI request cannot be honored.
///
/// Each maps into the door's own error enum, so a caller still matches one type.
#[derive(Debug, thiserror::Error)]
pub enum DriError {
    /// The side does not match the person's tier: the lawyer DRI is the
    /// accountable lawyer (`owner`/`admin`/`lawyer`), and the client DRI is the
    /// client-side contact.
    #[error("that person's tier cannot carry that side's accountability marker")]
    TierMismatch,
    /// This actor may not change that side's accountability on this matter.
    #[error("you may not change that side's accountability on this matter")]
    NotPermitted,
    /// The acting person has no `persons` row, so no rule can be applied to
    /// them. Distinct from [`NotPermitted`]: nothing was decided.
    ///
    /// [`NotPermitted`]: DriError::NotPermitted
    #[error("the acting person is not on file")]
    ActorUnknown,
    /// The last lawyer DRI cannot step off — a matter always has at least one.
    #[error("a matter always has a lawyer DRI; add another before removing this one")]
    LawyerDriRequired,
    #[error("database: {0}")]
    Db(String),
}

#[derive(Debug, Clone)]
pub struct AddParticipantCommand {
    pub project_id: Uuid,
    pub person_id: Uuid,
    /// The accountability marker this add designates, if any.
    pub dri: DriRequest,
    /// Who is asking. Consulted only when `dri` would move a marker.
    pub actor: DriActor,
}

#[derive(Debug, thiserror::Error)]
pub enum AddParticipantError {
    #[error("no such project")]
    ProjectNotFound,
    #[error("no such person")]
    PersonNotFound,
    #[error("that person is already assigned to this matter")]
    Duplicate,
    #[error(transparent)]
    Dri(DriError),
    #[error("database: {0}")]
    Db(String),
}

/// Whether this tier may carry that side's marker.
///
/// The lawyer DRI is the accountable lawyer, so it is the `owner`/`admin`/`lawyer`
/// tier — never a Clerk, who is a supervised non-lawyer, and never a client. The
/// client DRI is the client-side contact, which is the `client` tier.
fn tier_may_carry(person: &Person, side: DriSide) -> bool {
    match side {
        DriSide::Lawyer => person.role.is_lawyer_tier(),
        DriSide::Client => !person.role.is_lawyer_tier() && !person.role.is_clerk(),
    }
}

/// The marker write a validated request resolved to.
///
/// Carrying the direction as well as the side is what lets one audit row
/// describe the change without re-deriving it from the flags afterwards, by
/// which time they have already moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DriChange {
    side: DriSide,
    /// `true` to join the side's set, `false` to leave it.
    designating: bool,
}

/// Whether this actor may change that side's accountability on this matter.
///
/// The two sides answer to different people. The lawyer side is
/// **self-governing**: the lawyers already accountable for a matter are the ones
/// who decide who else is, so any current lawyer DRI may add or remove any
/// other, themselves included — bounded only by the rule that the set never
/// empties. The client side is the firm's call, so it takes the lawyer tier and
/// above; a client never designates their own counterpart, and a Clerk is a
/// supervised non-lawyer who designates neither.
///
/// Owner and Admin pass both. A matter whose lawyer set is empty has no holder
/// for the self-governing rule to admit, so the first lawyer DRI is named by a
/// lawyer-tier participant already on the matter, by Owner/Admin from outside
/// the ledger, or by [`DriActor::System`] at open time.
async fn authorize(
    application_store: &ApplicationStore,
    project_id: Uuid,
    actor: DriActor,
    side: DriSide,
) -> Result<(), DriError> {
    let DriActor::Person(actor_id) = actor else {
        return Ok(());
    };
    let acting = crate::persons::find_by_id(application_store, actor_id)
        .await
        .map_err(|error| DriError::Db(error.to_string()))?
        .ok_or(DriError::ActorUnknown)?;
    if acting.role.is_admin_tier() {
        return Ok(());
    }
    let permitted = match side {
        DriSide::Client => acting.role.is_lawyer_tier(),
        DriSide::Lawyer => {
            let current = holders(application_store, project_id, DriSide::Lawyer).await?;
            current.contains(&actor_id)
                || (current.is_empty()
                    && acting.role.is_lawyer_tier()
                    && actor_is_firm_participant(application_store, project_id, actor_id).await?)
        }
    };
    permitted.then_some(()).ok_or(DriError::NotPermitted)
}

/// Whether this person already sits on the firm side of the matter.
///
/// The empty lawyer-DRI set has no self-governing holder to admit a designation,
/// so the first marker is named by a lawyer-tier participant who is already on
/// the matter — not by someone reaching in from outside it.
async fn actor_is_firm_participant(
    application_store: &ApplicationStore,
    project_id: Uuid,
    actor_id: Uuid,
) -> Result<bool, DriError> {
    let Some(row) = projects::participation_for_person(application_store, actor_id, project_id)
        .await
        .map_err(|error| DriError::Db(error.to_string()))?
    else {
        return Ok(false);
    };
    Ok(!projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str()))
}

/// Validate a DRI request against the matter's current markers, before anything
/// is written. Returns the change to write, or `None` for a request that needs
/// no marker write at all.
///
/// `clearing` is the row whose marker is being dropped on a [`DriRequest::Clear`]
/// — `None` on the add door, where there is no existing row to clear.
async fn check_dri(
    application_store: &ApplicationStore,
    project_id: Uuid,
    person: &Person,
    actor: DriActor,
    request: DriRequest,
    clearing: Option<&PersonProjectRole>,
) -> Result<Option<DriChange>, DriError> {
    match request {
        DriRequest::Unchanged => Ok(None),
        DriRequest::Clear => {
            // Which side to leave is a fact about the row, not something the
            // caller names. A row carrying neither marker has nothing to clear.
            let Some(side) = clearing.and_then(|row| {
                if row.is_lawyer_dri {
                    Some(DriSide::Lawyer)
                } else if row.is_client_dri {
                    Some(DriSide::Client)
                } else {
                    None
                }
            }) else {
                return Ok(None);
            };
            if side == DriSide::Lawyer
                && holders(application_store, project_id, side).await?.len() <= 1
            {
                return Err(DriError::LawyerDriRequired);
            }
            authorize(application_store, project_id, actor, side).await?;
            Ok(Some(DriChange {
                side,
                designating: false,
            }))
        }
        DriRequest::Designate(side) => {
            if !tier_may_carry(person, side) {
                return Err(DriError::TierMismatch);
            }
            // Already accountable on this side: designation is additive, so
            // re-affirming someone changes nothing and records nothing.
            if holders(application_store, project_id, side)
                .await?
                .contains(&person.id)
            {
                return Ok(None);
            }
            authorize(application_store, project_id, actor, side).await?;
            Ok(Some(DriChange {
                side,
                designating: true,
            }))
        }
    }
}

/// Everyone carrying `side`'s marker on this matter.
///
/// A matter's accountability on each side is a set, so this is the shape every
/// rule above reads — "is it empty", "am I in it", "how many are left".
pub async fn holders(
    application_store: &ApplicationStore,
    project_id: Uuid,
    side: DriSide,
) -> Result<Vec<Uuid>, DriError> {
    let rows = projects::participations_for_project(application_store, project_id)
        .await
        .map_err(|error| DriError::Db(error.to_string()))?;
    Ok(rows
        .into_iter()
        .filter(|row| match side {
            DriSide::Lawyer => row.is_lawyer_dri,
            DriSide::Client => row.is_client_dri,
        })
        .map(|row| row.person_id)
        .collect())
}

/// Append the audit entry for one DRI change.
///
/// The matter is the subject, because "who has been accountable for this matter"
/// is the question the trail gets asked; the person moved is named in the
/// detail.
async fn record_dri_change(
    application_store: &ApplicationStore,
    project_id: Uuid,
    person_id: Uuid,
    actor: DriActor,
    change: DriChange,
) -> Result<(), DriError> {
    let action = match (change.side, change.designating) {
        (DriSide::Lawyer, true) => "lawyer_dri_designated",
        (DriSide::Lawyer, false) => "lawyer_dri_removed",
        (DriSide::Client, true) => "client_dri_designated",
        (DriSide::Client, false) => "client_dri_removed",
    };
    crate::relationship_logs::record(
        application_store,
        &crate::relationship_logs::NewRelationshipLog {
            actor_person_id: match actor {
                DriActor::Person(id) => Some(id),
                DriActor::System => None,
            },
            subject_type: "project".to_string(),
            subject_id: project_id,
            action: action.to_string(),
            detail: person_id.to_string(),
            project_id: Some(project_id),
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| DriError::Db(error.to_string()))
}

/// Write the validated DRI change and return this person's refreshed row.
///
/// **The audit entry lands first.** These are two writes and SurrealDB gives no
/// transaction across them, so one of them has to be able to fail alone. An
/// entry describing a change that did not happen is a discrepancy the trail
/// itself shows; a marker that moved with nothing recording it is the thing the
/// trail exists to prevent. Ordering it this way makes the recoverable failure
/// the possible one.
async fn apply_dri(
    application_store: &ApplicationStore,
    project_id: Uuid,
    person_id: Uuid,
    actor: DriActor,
    change: Option<DriChange>,
    row: PersonProjectRole,
) -> Result<PersonProjectRole, DriError> {
    let Some(change) = change else {
        return Ok(row);
    };
    record_dri_change(application_store, project_id, person_id, actor, change).await?;
    if change.designating {
        projects::designate_dri(application_store, project_id, person_id, change.side)
            .await
            .map_err(|error| DriError::Db(error.to_string()))?;
    } else {
        projects::clear_dri(application_store, row.id, change.side)
            .await
            .map_err(|error| DriError::Db(error.to_string()))?;
    }
    // The write rewrote the flags on this row, so the caller's copy is stale —
    // re-read it rather than hand back a row that lies about the marker.
    projects::participation_by_id(application_store, row.id)
        .await
        .map_err(|error| DriError::Db(error.to_string()))
        .map(|refreshed| refreshed.unwrap_or(row))
}

pub async fn add_participant(
    application_store: &ApplicationStore,
    input: &AddParticipantCommand,
) -> Result<PersonProjectRole, AddParticipantError> {
    let person = crate::persons::find_by_id(application_store, input.person_id)
        .await
        .map_err(|error| AddParticipantError::Db(error.to_string()))?
        .ok_or(AddParticipantError::PersonNotFound)?;
    if projects::participation_for_person(application_store, input.person_id, input.project_id)
        .await
        .map_err(db_add)?
        .is_some()
    {
        return Err(AddParticipantError::Duplicate);
    }
    // Every refusal lands before the first write, so a rejected designation
    // never leaves a bare participation row behind.
    let change = check_dri(
        application_store,
        input.project_id,
        &person,
        input.actor,
        input.dri,
        None,
    )
    .await
    .map_err(AddParticipantError::Dri)?;
    let participation = projects::participation_for_role(person.role);
    let row = projects::add_participation(
        application_store,
        input.project_id,
        input.person_id,
        participation,
    )
    .await
    .map_err(db_add)?;
    apply_dri(
        application_store,
        input.project_id,
        input.person_id,
        input.actor,
        change,
        row,
    )
    .await
    .map_err(AddParticipantError::Dri)
}

fn db_add(error: projects::ProjectStoreError) -> AddParticipantError {
    match error {
        projects::ProjectStoreError::NoSuchProject(_) => AddParticipantError::ProjectNotFound,
        projects::ProjectStoreError::NoSuchPerson(_) => AddParticipantError::PersonNotFound,
        projects::ProjectStoreError::DuplicateParticipation => AddParticipantError::Duplicate,
        other => AddParticipantError::Db(other.to_string()),
    }
}

#[derive(Debug, Clone)]
pub struct UpdateParticipantCommand {
    pub project_id: Uuid,
    pub role_id: Uuid,
    pub person_id: Uuid,
    /// The accountability marker this edit designates, clears, or leaves alone.
    pub dri: DriRequest,
    /// Who is asking. Consulted only when `dri` would move a marker.
    pub actor: DriActor,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateParticipantError {
    #[error("no such participation")]
    NotFound,
    #[error("no such person")]
    PersonNotFound,
    #[error("that person is already assigned to this matter")]
    Duplicate,
    #[error("that row is the matter's lawyer DRI and cannot be moved off the firm side")]
    DriLockout,
    #[error(transparent)]
    Dri(DriError),
    #[error("database: {0}")]
    Db(String),
}

pub async fn update_participant(
    application_store: &ApplicationStore,
    input: &UpdateParticipantCommand,
) -> Result<PersonProjectRole, UpdateParticipantError> {
    let existing = projects::participation_by_id(application_store, input.role_id)
        .await
        .map_err(db_update)?
        .filter(|role| role.project_id == input.project_id)
        .ok_or(UpdateParticipantError::NotFound)?;
    let person = crate::persons::find_by_id(application_store, input.person_id)
        .await
        .map_err(|error| UpdateParticipantError::Db(error.to_string()))?
        .ok_or(UpdateParticipantError::PersonNotFound)?;
    // Re-pointing the row re-derives its side of the matter. A lawyer DRI whose
    // incoming person is client-tier would flip client-side and strand the
    // matter's accountable lawyer, so the lockout reads the derived value.
    let participation = projects::participation_for_role(person.role);
    if existing.is_lawyer_dri
        && (existing.person_id != input.person_id
            || projects::PARTICIPATION_CLIENT_SIDE.contains(&participation))
    {
        return Err(UpdateParticipantError::DriLockout);
    }
    if existing.person_id != input.person_id
        && projects::participation_for_person(application_store, input.person_id, input.project_id)
            .await
            .map_err(db_update)?
            .is_some()
    {
        return Err(UpdateParticipantError::Duplicate);
    }
    // Validated before the update, so a refused designation leaves the row
    // pointing where it already pointed.
    let change = check_dri(
        application_store,
        input.project_id,
        &person,
        input.actor,
        input.dri,
        Some(&existing),
    )
    .await
    .map_err(UpdateParticipantError::Dri)?;
    let row = projects::update_participation(
        application_store,
        input.role_id,
        input.person_id,
        participation,
    )
    .await
    .map_err(db_update)?
    .ok_or(UpdateParticipantError::NotFound)?;
    apply_dri(
        application_store,
        input.project_id,
        input.person_id,
        input.actor,
        change,
        row,
    )
    .await
    .map_err(UpdateParticipantError::Dri)
}

fn db_update(error: projects::ProjectStoreError) -> UpdateParticipantError {
    match error {
        projects::ProjectStoreError::NoSuchPerson(_) => UpdateParticipantError::PersonNotFound,
        projects::ProjectStoreError::DuplicateParticipation => UpdateParticipantError::Duplicate,
        other => UpdateParticipantError::Db(other.to_string()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RemoveParticipantError {
    #[error("no such participation")]
    NotFound,
    #[error("that row is the matter's last lawyer DRI and cannot be removed")]
    DriLockout,
    #[error(transparent)]
    Dri(DriError),
    #[error("database: {0}")]
    Db(String),
}

/// Take one person off a matter.
///
/// Removing a participation that carries a marker is also a DRI change, so it
/// answers to the same two rules the designation door does: the lawyer set never
/// empties, and the actor has to be entitled to change that side. It is audited
/// on the same trail, for the same reason — a matter's accountability should not
/// be able to change through the door nobody was watching.
pub async fn remove_participant(
    application_store: &ApplicationStore,
    project_id: Uuid,
    role_id: Uuid,
    actor: DriActor,
) -> Result<(), RemoveParticipantError> {
    let existing = projects::participation_by_id(application_store, role_id)
        .await
        .map_err(|error| RemoveParticipantError::Db(error.to_string()))?
        .filter(|role| role.project_id == project_id)
        .ok_or(RemoveParticipantError::NotFound)?;
    let side = if existing.is_lawyer_dri {
        Some(DriSide::Lawyer)
    } else if existing.is_client_dri {
        Some(DriSide::Client)
    } else {
        None
    };
    if let Some(side) = side {
        if side == DriSide::Lawyer
            && holders(application_store, project_id, side)
                .await
                .map_err(RemoveParticipantError::Dri)?
                .len()
                <= 1
        {
            return Err(RemoveParticipantError::DriLockout);
        }
        authorize(application_store, project_id, actor, side)
            .await
            .map_err(RemoveParticipantError::Dri)?;
        // Audited before the row is gone, so the trail records the removal even
        // if the delete itself then fails — the same ordering `apply_dri` uses.
        record_dri_change(
            application_store,
            project_id,
            existing.person_id,
            actor,
            DriChange {
                side,
                designating: false,
            },
        )
        .await
        .map_err(RemoveParticipantError::Dri)?;
    }
    projects::remove_participation(application_store, role_id)
        .await
        .map_err(|error| RemoveParticipantError::Db(error.to_string()))
}

/// One `person_project_role` row whose participation predates ENG-478's
/// schema `ASSERT` — a word none of the five current tiers derive, and the
/// schema now refuses on any write going forward.
///
/// Carries the row id and the offending word, and nothing else. Never the
/// Project or the person: a Project code is a client identifier, and this
/// exists to be read by an operator, not pasted into an issue or a commit —
/// see the workspace's no-client-data rule.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnsupportedParticipation {
    pub role_id: Uuid,
    pub participation: String,
}

/// Whether the schema's `ASSERT` would admit this value today.
///
/// Reads straight off [`crate::persons::Role::parse`] so this report and the
/// schema's own vocabulary — both derived from the same five tiers — can
/// never quietly say something different from one another.
#[must_use]
fn is_supported_participation(value: &str) -> bool {
    crate::persons::Role::parse(value).is_some()
}

/// Find every row whose participation the schema would refuse if written
/// today. Pure, so a synthetic row proves the filter without writing a row
/// the schema now refuses to store.
#[must_use]
fn find_unsupported(rows: &[PersonProjectRole]) -> Vec<UnsupportedParticipation> {
    rows.iter()
        .filter(|row| !is_supported_participation(&row.participation))
        .map(|row| UnsupportedParticipation {
            role_id: row.id,
            participation: row.participation.clone(),
        })
        .collect()
}

/// Deployment-wide report of legacy participation values — rows a direct
/// write would be refused today, surviving only because they were written
/// before ENG-478 closed the vocabulary. Report-only: this never rewrites or
/// deletes a row. A legacy row's client-side handling
/// ([`projects::PARTICIPATION_CLIENT_SIDE`]) is unaffected by it existing;
/// reconciling one is a decision about a matter, not a mechanical fix.
pub async fn unsupported_participation_report(
    application_store: &ApplicationStore,
) -> Result<Vec<UnsupportedParticipation>, projects::ProjectStoreError> {
    let rows = projects::all_participations(application_store).await?;
    Ok(find_unsupported(&rows))
}

#[cfg(test)]
mod tests {
    use super::{find_unsupported, UnsupportedParticipation};

    use crate::projects::{self};

    use uuid::Uuid;

    // ── ENG-478: the legacy-value report ───────────────────────────────────

    /// A row with no Project or person on it — every field but the row id and
    /// the participation word is filler, because [`find_unsupported`] must
    /// never need them.
    fn synthetic_row(participation: &str) -> projects::PersonProjectRole {
        projects::PersonProjectRole {
            id: Uuid::now_v7(),
            person_id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            participation: participation.to_string(),
            is_lawyer_dri: false,
            is_client_dri: false,
            inserted_at: "2026-08-04T00:00:00Z".to_string(),
            updated_at: "2026-08-04T00:00:00Z".to_string(),
        }
    }

    /// Every one of the five words the schema's `ASSERT` admits reports
    /// nothing — the report and the schema must agree on what is current.
    #[test]
    fn every_role_derived_value_reports_as_supported() {
        let rows: Vec<_> = ["owner", "admin", "lawyer", "clerk", "client"]
            .map(synthetic_row)
            .into();
        assert_eq!(find_unsupported(&rows), Vec::new());
    }

    /// A retired word — never written by any door today — is exactly what
    /// the report exists to surface, carrying only the row id and the word.
    #[test]
    fn a_legacy_word_is_reported_with_no_project_or_person() {
        let legacy = synthetic_row("counterparty");
        let reported = find_unsupported(std::slice::from_ref(&legacy));

        assert_eq!(
            reported,
            vec![UnsupportedParticipation {
                role_id: legacy.id,
                participation: "counterparty".to_string(),
            }]
        );
    }

    /// A deployment-wide scan reports only the rows that need it, mixed in
    /// among current ones — proving the filter, not just one row at a time.
    #[test]
    fn the_report_finds_only_the_unsupported_rows_in_a_mixed_set() {
        let current = synthetic_row("lawyer");
        let legacy_one = synthetic_row("attorney");
        let legacy_two = synthetic_row("paralegal");
        let rows = vec![current, legacy_one.clone(), legacy_two.clone()];

        let mut reported = find_unsupported(&rows);
        reported.sort_by_key(|row| row.participation.clone());

        assert_eq!(
            reported,
            vec![
                UnsupportedParticipation {
                    role_id: legacy_one.id,
                    participation: "attorney".to_string(),
                },
                UnsupportedParticipation {
                    role_id: legacy_two.id,
                    participation: "paralegal".to_string(),
                },
            ]
        );
    }
}
