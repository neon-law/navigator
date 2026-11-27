//! Shared People command layer for every write adapter.
//!
//! Every People mutation — create, update, delete — and the fuzzy
//! People lookup live here so the JSON `/app/api/people*` surface, the
//! browser lawyer forms, and the Navigator MCP tools travel one command
//! boundary. This crate owns the persistence and business rules
//! (validation, role rules, the bootstrap-owner guards, duplicate-email
//! conflicts); the adapters render and authorize but never re-implement
//! the write. This module carries no HTTP or email machinery, so `mcp`
//! and `cli` can call it directly; the welcome-email command, which
//! needs the mailer, stays in `web`.

use serde::Deserialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::external_identities::{self, ExternalIdentityError, ExternalSystem};
use crate::persons::{self, ContactUpdate, NewPerson, Person, PersonEdit, PersonError, Role};

#[path = "person_creation_scope.rs"]
mod creation_scope;

/// Authorize Person creation against the actor's current Project and Firm access.
/// The resolved Firm is context only: creating a Person grants no matter access.
pub async fn authorize_creation(
    db: &ApplicationStore,
    input: &CreatePersonCommand,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    project_id: Option<Uuid>,
) -> Result<Option<Uuid>, PeopleCommandError> {
    if let Some(message) = input.validation_message() {
        return Err(PeopleCommandError::Invalid(message));
    }
    if !actor_role.is_lawyer_tier() {
        return Err(PeopleCommandError::Blocked(
            "Person creation requires Lawyer, Admin or Owner authority.",
        ));
    }
    let requested = parse_create_role(&input.role);
    let mut firm_id = input.firm_id;
    if actor_role == Role::Admin
        && requested != Role::Client
        && firm_id.is_none()
        && project_id.is_none()
    {
        let person_id = actor_person_id.ok_or(PeopleCommandError::NotFound)?;
        let memberships = crate::firms::memberships_for_person(db, person_id)
            .await
            .map_err(|error| PeopleCommandError::Dependency(error.to_string()))?;
        let mut administered = memberships
            .iter()
            .filter(|row| row.membership == crate::firms::FirmMembership::Admin);
        firm_id = administered.next().map(|row| row.firm_id);
        if administered.next().is_some() {
            return Err(PeopleCommandError::Invalid(
                "Select an administered Firm explicitly.",
            ));
        }
    }
    if let Some(project_id) = project_id {
        if actor_role == Role::Lawyer {
            let visible = crate::access::can_see_project_as_lawyer(
                db,
                actor_person_id,
                actor_role,
                project_id,
            )
            .await
            .map_err(PeopleCommandError::Dependency)?;
            if !visible {
                return Err(PeopleCommandError::NotFound);
            }
        }
        let project = crate::projects::find_by_id(db, project_id)
            .await
            .map_err(|error| PeopleCommandError::Dependency(error.to_string()))?
            .ok_or(PeopleCommandError::NotFound)?;
        if firm_id.is_some() && firm_id != project.firm_id {
            return Err(PeopleCommandError::NotFound);
        }
        firm_id = project.firm_id;
    }
    if !creation_scope::permits(actor_role, requested, firm_id.is_some()) {
        return Err(PeopleCommandError::Blocked(
            "Person creation requires an authorized Project or Firm and an allowed role.",
        ));
    }
    if actor_role.is_admin_tier() {
        if let Some(firm_id) = firm_id {
            let capability = crate::firm_capability::resolve(
                db,
                actor_role,
                actor_person_id,
                firm_id,
                crate::firm_capability::FirmCapability::ManageMembership,
            )
            .await
            .map_err(|error| PeopleCommandError::Dependency(error.to_string()))?;
            if !capability.is_allowed() {
                return Err(PeopleCommandError::NotFound);
            }
        }
    }
    Ok(firm_id)
}

/// Authorize and create through the same boundary for the API and native forms.
pub async fn create_person_for_actor(
    db: &ApplicationStore,
    mut input: CreatePersonCommand,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    project_id: Option<Uuid>,
) -> Result<Person, PeopleCommandError> {
    input.firm_id = authorize_creation(db, &input, actor_role, actor_person_id, project_id).await?;
    create_person(db, &input).await
}

/// Request body for creating a Person through the command boundary.
#[derive(Debug, Deserialize)]
pub struct CreatePersonCommand {
    pub name: String,
    pub email: String,
    /// `owner`, `admin`, `lawyer`, `clerk`, or `client`. Missing or blank values fall
    /// back to `client`.
    #[serde(default)]
    pub role: String,
    /// Structured legal-name parts for filings that split the name.
    #[serde(default)]
    pub given_name: Option<String>,
    #[serde(default)]
    pub family_name: Option<String>,
    #[serde(default)]
    pub middle_name: Option<String>,
    /// The stable Notion workspace user id. Stored in the external-identity
    /// table, never on the Person row.
    #[serde(default)]
    pub notion_user_id: Option<String>,
    /// The Firm a new Admin, Lawyer, or Clerk joins. Admin requires an
    /// explicit Firm to gain membership. Lawyer and Clerk default to the
    /// deployment's anchor Firm. Client and Owner do not join here.
    #[serde(default)]
    pub firm_id: Option<Uuid>,
}

impl CreatePersonCommand {
    #[must_use]
    pub fn validation_message(&self) -> Option<&'static str> {
        validate_name_email(&self.name, &self.email).or_else(|| role_validation_message(&self.role))
    }
}

/// Request body for updating a Person through the command boundary.
///
/// Every field is optional, and an absent one leaves its column exactly as
/// it was — `name` and `email` included, matching the partial-update
/// contract [`crate::projects::UpdateProjectCommand`] documents. A caller
/// that wants to correct only `linkedin_url`, say, sends only that field.
/// `#[serde(deny_unknown_fields)]` refuses a field this struct does not
/// name rather than silently dropping it.
///
/// A blank/absent `role` preserves the row's existing role rather than
/// resetting it. The structured name parts use a **double option** so an
/// *omitted* field (outer `None`) is left untouched, while a *present*
/// field — whether JSON `null`, a blank string, or a value — is applied:
/// `null`/blank clear the column, a value sets it. That lets a JSON
/// client following the nullable schema clear a stale legal-name part,
/// which a single `Option` (where `null` and "omitted" collapse to the
/// same `None`) could not express.
#[derive(Debug, Default, Deserialize)]
// `Option<Option<String>>` is deliberate here: the outer option is
// "field present?" and the inner is "null vs a value", which is exactly
// the PATCH clear-vs-preserve distinction. That's the sanctioned use the
// `option_option` lint warns is usually a mistake — it isn't one here.
#[allow(clippy::option_option)]
#[serde(deny_unknown_fields)]
pub struct UpdatePersonCommand {
    /// Absent leaves the name unchanged; present but blank is refused — a
    /// person with no name is not a state a patch may produce.
    #[serde(default)]
    pub name: Option<String>,
    /// Absent leaves the email unchanged. The bootstrap Owner's email is
    /// pinned regardless of what is submitted here (see
    /// [`update_person`]), so a missing key is never mistaken for a
    /// blank-out on that row either. Present but blank is refused, the
    /// same rule `name` follows — an email is required to contain an `@`.
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub role: String,
    #[serde(default, deserialize_with = "double_option")]
    pub given_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub family_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub middle_name: Option<Option<String>>,
    /// Omit to preserve the Notion identity; send null or blank to clear it.
    #[serde(default, deserialize_with = "double_option")]
    pub notion_user_id: Option<Option<String>>,
    /// The public `LinkedIn` profile URL shown on `/team`. Omit to preserve
    /// it; send null or blank to clear it.
    #[serde(default, deserialize_with = "double_option")]
    pub linkedin_url: Option<Option<String>>,
    /// The browser form's CSRF token, present on every native
    /// `/app/admin/people/{id}` submission alongside the fields above —
    /// this command boundary serves that form directly, with no separate
    /// form-shaped DTO. Declared and ignored here so
    /// `deny_unknown_fields` refuses a genuinely unrecognized field
    /// without also refusing the one field every browser post carries
    /// that isn't part of the write itself.
    #[serde(default, rename = "_csrf")]
    pub csrf_token: Option<String>,
}

/// Deserialize a "double option" that keeps a present JSON `null`
/// distinct from an absent field. `#[serde(default)]` supplies the outer
/// `None` when the key is missing; this runs only when the key is
/// present, so a bare derive's `null → None` becomes `null → Some(None)`
/// (clear) and a value becomes `Some(Some(value))` (set).
#[allow(clippy::option_option)] // the doubled option is the whole point — see the DTO.
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// The per-request authorization facts an update needs, resolved from
/// the caller's session/config by the adapter (web route or API handler)
/// so the command itself stays free of session types.
#[derive(Debug, Clone, Copy)]
pub struct UpdateContext<'a> {
    /// The configured `NAVIGATOR_BOOTSTRAP_OWNER_EMAIL`, if any. When it
    /// matches the target row the role is pinned to `owner` no matter
    /// what the request body carries, so an accidental (or hostile)
    /// demotion can't lock out the grant.
    pub bootstrap_owner_email: Option<&'a str>,
    /// The caller's stored system role. A caller cannot edit a higher-ranked
    /// person, even when the surrounding route is otherwise writable.
    pub actor_role: Role,
    /// The caller may set roles (Owner/Admin, not in a read-only DRI view).
    /// When false, a submitted role is ignored and the existing role is
    /// preserved.
    pub may_change_roles: bool,
}

#[derive(Debug)]
pub enum PeopleCommandError {
    Invalid(&'static str),
    /// The email belongs to another person. Raised by
    /// [`PersonError::EmailTaken`], which the `person_email_lower`
    /// unique index produces.
    EmailConflict,
    /// No Person with the requested id.
    NotFound,
    /// The action is refused by a business rule (e.g. deleting the
    /// bootstrap Owner). Carries a human-readable reason.
    Blocked(&'static str),
    /// The welcome email could not be dispatched. The row exists; the
    /// send failed. Constructed by the `web` welcome command.
    SendFailed,
    Db(PersonError),
    /// A Notion identity belongs to another Person, or the external identity
    /// store rejected the update. No identity value is included in the user
    /// message.
    ExternalIdentity,
    /// ENG-495: granting the default Firm membership on create failed for a
    /// reason other than a bad `firm_id` (mapped to [`Self::Invalid`]
    /// instead, since a caller naming one can correct it).
    FirmMembership(String),
    /// ENG-928: [`delete_person`]'s participation guard could not even read
    /// `person_project_role` to check it. Distinct from [`Self::Db`] because
    /// that variant is typed to [`PersonError`], not
    /// [`crate::projects::ProjectStoreError`].
    Dependency(String),
    /// ENG-928: refused because a `person_project_role` row still names this
    /// Person on a Project. Carries how many rows, so the refusal names what
    /// still needs clearing — the same shape as
    /// `entity_commands::EntityCommandError::Referenced`. Guards the root
    /// cause of a DRI participation surviving its Person: a deleted row can
    /// no longer leave one behind, so `store::project_surfaces::read_handles`
    /// only ever meets one from data written before this guard existed.
    ParticipatesInProjects(usize),
}

impl PeopleCommandError {
    /// The message to show a human — a toast on the People forms, or the
    /// error banner in the inline client-create modal. Kept here so every
    /// adapter renders the same wording per failure.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            PeopleCommandError::Invalid(m) | PeopleCommandError::Blocked(m) => (*m).to_string(),
            PeopleCommandError::EmailConflict => "That email is already in use.".to_string(),
            PeopleCommandError::NotFound => "That person no longer exists.".to_string(),
            PeopleCommandError::SendFailed => {
                "Couldn't send the welcome email. Check the email log.".to_string()
            }
            PeopleCommandError::Db(_) => "Something went wrong. Please try again.".to_string(),
            PeopleCommandError::ExternalIdentity => {
                "That Notion user is already linked to another person.".to_string()
            }
            PeopleCommandError::FirmMembership(_) | PeopleCommandError::Dependency(_) => {
                "Something went wrong. Please try again.".to_string()
            }
            PeopleCommandError::ParticipatesInProjects(count) => format!(
                "This person still participates on {count} Project{}; remove them from every Project before deleting.",
                if *count == 1 { "" } else { "s" }
            ),
        }
    }
}

/// A trimmed form value, or `None` when it is blank.
#[must_use]
pub fn none_if_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[must_use]
pub fn parse_role(s: &str) -> Option<Role> {
    Role::parse(s)
}

/// `true` when `configured` names the same email as `row_email`,
/// case-insensitively — i.e. the row is the bootstrap Owner.
#[must_use]
pub fn is_bootstrap_owner_email(configured: Option<&str>, row_email: &str) -> bool {
    matches!(configured, Some(e) if e.eq_ignore_ascii_case(row_email))
}

/// Shared name/email shape check: name required, email must contain an
/// `@` and carry no internal whitespace.
fn validate_name_email(name: &str, email: &str) -> Option<&'static str> {
    let email = email.trim();
    if name.trim().is_empty() || !email.contains('@') || email.chars().any(char::is_whitespace) {
        Some("Name is required and email must contain an @.")
    } else {
        None
    }
}

fn role_validation_message(s: &str) -> Option<&'static str> {
    match s.trim() {
        "" | "owner" | "admin" | "lawyer" | "clerk" | "client" => None,
        _ => Some("Role must be owner, admin, lawyer, clerk, or client."),
    }
}

/// A blank or unknown role on a create falls back to `client`. The
/// validation above has already rejected an unknown one, so this only
/// ever resolves the blank.
fn parse_create_role(s: &str) -> Role {
    Role::parse(s).unwrap_or_default()
}

/// Map a write failure to the right command error: the email conflict is
/// caller-correctable, everything else a server-side fault. The OIDC
/// conflict cannot reach here — nothing in this boundary writes
/// `oidc_subject`.
fn classify_write(error: PersonError) -> PeopleCommandError {
    match error {
        PersonError::EmailTaken => PeopleCommandError::EmailConflict,
        other => PeopleCommandError::Db(other),
    }
}

fn classify_external_identity(_error: ExternalIdentityError) -> PeopleCommandError {
    PeopleCommandError::ExternalIdentity
}

fn classify_firm_write(error: crate::firms::FirmError) -> PeopleCommandError {
    match error {
        crate::firms::FirmError::NoSuchFirm(_) => PeopleCommandError::Invalid("No such firm."),
        other => PeopleCommandError::FirmMembership(other.to_string()),
    }
}

/// Grant Firm membership at creation. Only Lawyer and Clerk default to the anchor Firm.
async fn grant_default_firm_membership(
    db: &ApplicationStore,
    person: &Person,
    firm_id: Option<Uuid>,
) -> Result<(), PeopleCommandError> {
    let Some(membership) = crate::firms::FirmMembership::for_role(person.role) else {
        return Ok(());
    };
    if person.role == Role::Admin && firm_id.is_none() {
        return Ok(());
    }
    let firm_id = match firm_id {
        Some(id) => Some(id),
        None => crate::firms::anchor_firm(db)
            .await
            .map_err(classify_firm_write)?
            .map(|firm| firm.id),
    };
    let Some(firm_id) = firm_id else {
        return Ok(());
    };
    crate::firms::ensure_membership(
        db,
        &crate::firms::NewPersonFirmRole {
            person_id: person.id,
            firm_id,
            membership,
            is_dri: false,
        },
    )
    .await
    .map_err(classify_firm_write)
}

pub async fn create_person(
    db: &ApplicationStore,
    input: &CreatePersonCommand,
) -> Result<Person, PeopleCommandError> {
    if let Some(message) = input.validation_message() {
        return Err(PeopleCommandError::Invalid(message));
    }
    let created = persons::create(
        db,
        &NewPerson {
            role: parse_create_role(&input.role),
            given_name: none_if_blank(input.given_name.as_deref()),
            family_name: none_if_blank(input.family_name.as_deref()),
            middle_name: none_if_blank(input.middle_name.as_deref()),
            ..NewPerson::new(input.name.trim(), input.email.trim())
        },
    )
    .await
    .map_err(classify_write)?;
    if input.notion_user_id.is_some() {
        if let Err(error) = external_identities::set_for_person(
            db,
            created.id,
            ExternalSystem::Notion,
            input.notion_user_id.as_deref(),
        )
        .await
        {
            let _ = persons::delete(db, created.id).await;
            return Err(classify_external_identity(error));
        }
    }
    if let Err(error) = grant_default_firm_membership(db, &created, input.firm_id).await {
        let _ = persons::delete(db, created.id).await;
        return Err(error);
    }
    Ok(created)
}

/// Update one Person by id. Every field is a partial update: an absent
/// `name` or `email` leaves the column unchanged, and a present-but-blank
/// one is refused (a person cannot go nameless, and an email must contain
/// an `@`). The bootstrap Owner's email and role are pinned on top of
/// that — any submitted change to either is silently dropped rather than
/// rejecting the whole write — while its name and legal-name parts edit
/// normally. Otherwise rejects callers attempting to edit a higher-ranked
/// person, and preserves the existing role when the caller can't change
/// roles or submits a blank role; leaves an omitted structured-name part
/// untouched and nulls a present-but-blank one.
pub async fn update_person(
    db: &ApplicationStore,
    id: Uuid,
    input: &UpdatePersonCommand,
    ctx: &UpdateContext<'_>,
) -> Result<Person, PeopleCommandError> {
    let existing = persons::find_by_id(db, id)
        .await
        .map_err(PeopleCommandError::Db)?
        .ok_or(PeopleCommandError::NotFound)?;

    // The bootstrap Owner's identity is pinned: the email that keys the
    // `NAVIGATOR_BOOTSTRAP_OWNER_EMAIL` carve-out (and, by extension, its
    // role) cannot move through this boundary. A submitted change to either
    // is silently dropped — the same preserve-not-error pattern the
    // `may_change_roles` branch below already uses for a caller without role
    // authority — rather than rejecting the whole write, since name and the
    // legal-name parts are not what `oauth::resolve_person_from_claims` keys
    // its re-tag on and edit like any other person's.
    let is_bootstrap = is_bootstrap_owner_email(ctx.bootstrap_owner_email, &existing.email);
    let email = if is_bootstrap {
        existing.email.clone()
    } else {
        input
            .email
            .as_deref()
            .map_or_else(|| existing.email.clone(), |email| email.trim().to_string())
    };
    // Absent leaves the name unchanged, like every other omitted field here;
    // present-but-blank falls through to the validation below, which
    // refuses it the same way it always has.
    let name = input
        .name
        .as_deref()
        .map_or_else(|| existing.name.clone(), |name| name.trim().to_string());

    if let Some(message) = validate_name_email(&name, &email) {
        return Err(PeopleCommandError::Invalid(message));
    }
    if existing.role.authority_rank() > ctx.actor_role.authority_rank() {
        return Err(PeopleCommandError::Blocked(
            "You cannot edit a person with a higher system role.",
        ));
    }

    let new_role = if is_bootstrap {
        Role::Owner
    } else if !ctx.may_change_roles || input.role.trim().is_empty() {
        existing.role
    } else {
        let requested = parse_role(&input.role).ok_or(PeopleCommandError::Invalid(
            "Role must be owner, admin, lawyer, clerk, or client.",
        ))?;
        if requested.authority_rank() > ctx.actor_role.authority_rank() {
            return Err(PeopleCommandError::Blocked(
                "You cannot assign a system role above your own.",
            ));
        }
        if requested == Role::Admin && existing.role != Role::Admin && !ctx.actor_role.is_owner() {
            return Err(PeopleCommandError::Blocked(
                "Only an Owner can assign the Admin role.",
            ));
        }
        requested
    };

    let updated = persons::edit(
        db,
        id,
        &PersonEdit {
            name: Some(name),
            email: Some(email),
            role: Some(new_role),
            // Only touch a name part when the request carried it (outer
            // `Some`), so a caller that posts just name/email/role leaves
            // the structured legal name untouched rather than nulling it
            // out from under a future N-400. A present `null`/blank
            // clears the column; a value sets it.
            given_name: input
                .given_name
                .as_ref()
                .map(|part| none_if_blank(part.as_deref())),
            family_name: input
                .family_name
                .as_ref()
                .map(|part| none_if_blank(part.as_deref())),
            middle_name: input
                .middle_name
                .as_ref()
                .map(|part| none_if_blank(part.as_deref())),
            profile_image_url: None,
            linkedin_url: input
                .linkedin_url
                .as_ref()
                .map(|part| none_if_blank(part.as_deref())),
        },
    )
    .await
    .map_err(classify_write)?
    .ok_or(PeopleCommandError::NotFound)?;

    if let Some(notion_user_id) = input.notion_user_id.as_ref() {
        external_identities::set_for_person(
            db,
            id,
            ExternalSystem::Notion,
            notion_user_id.as_deref(),
        )
        .await
        .map_err(classify_external_identity)?;
    }
    Ok(updated)
}

/// Delete one Person by id. Only **client** records are deletable: a
/// lawyer can't delete another privileged person, so a non-client
/// target is refused here at the command boundary (the lawyer People list
/// also hides the control, but this is the enforcing check a hand-crafted
/// `DELETE` still hits). The configured bootstrap Owner is undeletable on
/// top of that — without the guard an admin could wipe the row that
/// `oauth::resolve_person_from_claims` re-tags on every login, locking out
/// the grant.
///
/// ENG-928: also refused while any `person_project_role` row still names
/// this Person — as a DRI or as an ordinary participant. A client is
/// routinely a Project's `client_dri`; deleting them out from under that
/// row is exactly how `store::project_surfaces::read_handles` met a DRI
/// participation pointing at a missing Person and 500'd. The caller must
/// reassign or remove the participation first, the same shape
/// `entity_commands::delete_entity` already uses for a Project still
/// naming an Entity. Returns the deleted row on success.
pub async fn delete_person(
    db: &ApplicationStore,
    id: Uuid,
    bootstrap_owner_email: Option<&str>,
) -> Result<Person, PeopleCommandError> {
    let target = persons::find_by_id(db, id)
        .await
        .map_err(PeopleCommandError::Db)?
        .ok_or(PeopleCommandError::NotFound)?;
    if is_bootstrap_owner_email(bootstrap_owner_email, &target.email) {
        return Err(PeopleCommandError::Blocked(
            "Cannot delete the bootstrap Owner person (configured via NAVIGATOR_BOOTSTRAP_OWNER_EMAIL).",
        ));
    }
    if target.role != Role::Client {
        return Err(PeopleCommandError::Blocked(
            "Only client records can be deleted. Owner, admin, lawyer, and clerk people are edit-only.",
        ));
    }
    let participations = crate::projects::participations_for_person(db, id)
        .await
        .map_err(|error| PeopleCommandError::Dependency(error.to_string()))?;
    if !participations.is_empty() {
        return Err(PeopleCommandError::ParticipatesInProjects(
            participations.len(),
        ));
    }
    persons::delete(db, id)
        .await
        .map_err(PeopleCommandError::Db)?;
    Ok(target)
}

/// Fuzzy-find people by an optional name and/or email substring. Both
/// needles are matched case-insensitively as substrings and ANDed when
/// both are supplied; the caller is responsible for rejecting the
/// all-`None` case (a blank query would return the whole directory).
/// Results are ordered by name and capped at `limit`.
///
/// This is the read half of the People command boundary: the Navigator MCP
/// `show_person` tool and any web lookup share this one query
/// instead of re-implementing the `LIKE` predicate.
pub async fn search_people(
    db: &ApplicationStore,
    name: Option<&str>,
    email: Option<&str>,
    limit: u64,
) -> Result<Vec<Person>, PersonError> {
    persons::search(db, name, email, limit).await
}

/// Apply a directory import's contact facts. The import's own
/// find-or-create decides *which* row; this is the write half, and it
/// deliberately cannot reach `email` or `role`.
///
/// # Errors
///
/// [`PeopleCommandError::NotFound`] when the person no longer exists.
pub async fn update_person_contact(
    db: &ApplicationStore,
    id: Uuid,
    input: &ContactUpdate,
) -> Result<Person, PeopleCommandError> {
    persons::update_contact(db, id, input)
        .await
        .map_err(classify_write)?
        .ok_or(PeopleCommandError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::UpdatePersonCommand;

    // ── ENG-495: a newly created Lawyer or Clerk joins a Firm ──────────────

    /// `deny_unknown_fields` refuses a body naming a field this command does
    /// not carry, and the error names it — never the caller-opaque
    /// "Malformed JSON body." a missing-field rejection used to share with
    /// every other deserialize failure.
    #[test]
    fn an_unrecognized_field_is_refused_naming_it() {
        let error =
            serde_json::from_str::<UpdatePersonCommand>(r#"{"bogus_field": true}"#).unwrap_err();
        assert!(error.to_string().contains("bogus_field"), "{error}");
    }
}
