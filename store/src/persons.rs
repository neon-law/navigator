//! The PostgreSQL person directory and the authorization tier read at sign-in.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{future::Future, pin::Pin};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
pub use model::persons::Role;

/// One person in the directory.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`PersonRow`] is the seam that turns it into (and back out of) what
/// the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Person {
    pub id: Uuid,
    /// The display name. The structured legal-name parts below are what
    /// a filing that must split the name reads.
    pub name: String,
    /// The person's given (first) name. `None` until a matter needs the
    /// legal name split into parts.
    pub given_name: Option<String>,
    /// The person's family (last) name. `None` until set.
    pub family_name: Option<String>,
    /// The person's middle name, if any. `None` until set, and `None`
    /// for a person with no middle name.
    pub middle_name: Option<String>,
    /// The mailbox, as supplied. Matching is case-insensitive through
    /// the stored `email_lower` field — see [`find_by_email_ci`].
    pub email: String,
    /// Primary-provider `sub` claim — stable identifier from the IdP
    /// (Rauthy, Google, etc.). `None` for seeded persons not yet linked.
    pub oidc_subject: Option<String>,
    /// Microsoft subject, when this Person has linked that provider.
    pub microsoft_subject: Option<String>,
    /// Apple subject, when this Person has linked that provider.
    pub apple_subject: Option<String>,
    /// System-wide tier.
    pub role: Role,
    /// The contact's role at their organization (e.g. "Executive
    /// Director"). Free text; `None` until set by the importer or an
    /// admin edit.
    pub title: Option<String>,
    /// The contact's direct phone line. `None` until set.
    pub phone: Option<String>,
    /// Xero `ContactID` (GUID) once this person has been mirrored to
    /// Xero Contacts via the billing seam (one-way, Neon Law Navigator →
    /// Xero). `None` until first synced.
    pub xero_contact_id: Option<String>,
    /// This person's avatar. Firm-tier native uploads are public asset URLs
    /// for the canonical `people/{id}/avatar.{png,jpg}` object; their profile
    /// form discloses this before it submits. Client and historical rows hold
    /// a bare private documents-bucket key
    /// (`people/{id}/avatars/…`), while directory-sync may supply another
    /// directly-fetchable external URL. Application avatar routes retain their
    /// own viewer checks for the former private shape.
    pub profile_image_url: Option<String>,
    /// Optional `LinkedIn` profile URL, shown on the admin Person page.
    /// `None` until set by an admin edit.
    pub linkedin_url: Option<String>,
    /// Whether this person's email has been confirmed — either by the IdP
    /// carrying `email_verified: true` at sign-in, or by completing the
    /// non-Google confirm-email flow. Defaults to `false`; the public
    /// `/team` page shows only confirmed, non-client people. See
    /// [`set_email_confirmed`].
    pub email_confirmed: bool,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Errors reading or writing a person.
#[derive(Debug, thiserror::Error)]
pub enum PersonError {
    /// A database operation failed.
    /// PostgreSQL People authority is configured but unavailable or a query failed.
    #[error("PostgreSQL People authority: {0}")]
    Postgres(String),
    /// The write collided with `person_email_lower` — another row
    /// already holds this mailbox, case-insensitively.
    #[error("that email is already in use")]
    EmailTaken,
    /// The write collided with `person_oidc_subject` — another row is
    /// already linked to this IdP identity.
    #[error("that IdP identity is already linked to another person")]
    OidcSubjectTaken,
    /// The write collided with `person_microsoft_subject` — another row is
    /// already linked to this Microsoft identity.
    #[error("that Microsoft identity is already linked to another person")]
    MicrosoftSubjectTaken,
    /// The write collided with `person_apple_subject` — another row is
    /// already linked to this Apple identity.
    #[error("that Apple identity is already linked to another person")]
    AppleSubjectTaken,
    /// A write reported success but returned no row, or returned one
    /// this module could not read back — see [`PersonRow::into_person`].
    #[error("writing a person returned no usable row")]
    WriteReturnedNothing,
    /// Reading a structured Person Phone failed.
    #[error("phone lookup: {0}")]
    Phone(String),
}

/// Application identity reads and writes require validated PostgreSQL history.
/// The explicit importer context alone reads Source rows.
pub(crate) fn postgres_authority(
) -> Pin<Box<dyn Future<Output = Result<crate::postgres::PostgresDb, PersonError>> + Send>> {
    Box::pin(async {
        crate::postgres::PostgresDb::application()
            .await
            .map_err(|_| {
                PersonError::Postgres("PostgreSQL identity authority is unavailable".into())
            })
    })
}

/// The person currently holding `email_lower`, or `None` when the mailbox
/// is free.
///
/// A **direct record read**, deliberately not a scan. That is not a
/// micro-optimisation: a `WHERE email_lower = $v` scan is what the forked
/// shape did, and a scan does not enter the transaction's read set, so it
/// cannot make one racer observe another's write.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn mailbox_holder(
    _db: &ApplicationStore,
    email_lower: &str,
) -> Result<Option<Uuid>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::mailbox_holder(&pg, email_lower)).await
}

/// The fields a new person row carries. Everything but `name` and
/// `email` has a sensible empty default, so a caller that only knows
/// those two writes them and takes the rest.
#[derive(Debug, Clone, Default)]
pub struct NewPerson {
    pub name: String,
    pub email: String,
    /// Defaults to [`Role::Client`]: promotion is always opt-in.
    pub role: Role,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub middle_name: Option<String>,
    pub oidc_subject: Option<String>,
    pub microsoft_subject: Option<String>,
    pub apple_subject: Option<String>,
    pub title: Option<String>,
    pub phone: Option<String>,
    pub profile_image_url: Option<String>,
    pub linkedin_url: Option<String>,
}

impl NewPerson {
    /// The common case: a display name and a mailbox, everything else
    /// defaulted.
    #[must_use]
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: email.into(),
            ..Self::default()
        }
    }

    /// The same, at an explicit authority tier.
    #[must_use]
    pub fn with_role(name: impl Into<String>, email: impl Into<String>, role: Role) -> Self {
        Self {
            role,
            ..Self::new(name, email)
        }
    }
}

/// The fields an edit may change, in the shape the People command
/// boundary needs: a `None` leaves the column alone, and the doubled
/// option on a structured name part keeps "clear it" distinct from
/// "don't touch it" — the PATCH distinction a single option collapses.
#[derive(Debug, Clone, Default)]
// The doubled option is the whole point — see the doc comment.
#[allow(clippy::option_option)]
pub struct PersonEdit {
    pub name: Option<String>,
    pub email: Option<String>,
    pub role: Option<Role>,
    pub given_name: Option<Option<String>>,
    pub family_name: Option<Option<String>>,
    pub middle_name: Option<Option<String>>,
    /// The public avatar URL for a firm-tier Person's photo, or `None` when
    /// they have none. Client and historical rows retain a private documents-bucket key
    /// (`people/{id}/avatars/…`). A seed reconciliation may replace it while
    /// leaving identity and authority untouched; browser-facing commands do
    /// not populate this field directly, so their existing PATCH contract is
    /// unchanged.
    pub profile_image_url: Option<Option<String>>,
    /// The person's `LinkedIn` profile URL, shown on their admin Person page.
    /// Edited through the ordinary admin Person form, same
    /// clear-vs-leave-alone semantics as
    /// [`profile_image_url`](Self::profile_image_url).
    pub linkedin_url: Option<Option<String>>,
}

/// The contact facts a directory import owns: the display name and the
/// two free-text contact columns. Separate from [`PersonEdit`] because
/// an import must never touch `role` or `email` — the row it found is
/// the row it updates, and a person promoted to lawyer stays promoted.
#[derive(Debug, Clone, Default)]
pub struct ContactUpdate {
    pub name: String,
    pub title: Option<String>,
    pub phone: Option<String>,
}

/// Resolve a person by id.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn find_by_id(_db: &ApplicationStore, id: Uuid) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::by_id(&pg, id)).await
}

/// Resolve several people at once, for a caller holding a batch of
/// `person_id`s from a table that has not moved yet. Ids with no row are
/// simply absent from the result, so the caller sees exactly who exists.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn find_by_ids(_db: &ApplicationStore, ids: &[Uuid]) -> Result<Vec<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::many_by_ids(&pg, ids)).await
}

/// Resolve a person by `email`, matched case-insensitively.
///
/// Email is a case-insensitive identifier: `Attorney@Example.com` and
/// `attorney@example.com` are the same mailbox, and an IdP may present a
/// casing that differs from the stored row. Every lookup keyed on email
/// goes through here, filtering the stored `email_lower` field so it
/// agrees with the `person_email_lower` unique index by construction —
/// the engine rejects an expression index, so lowercasing in the
/// predicate instead would leave the lookup and the constraint able to
/// disagree.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn find_by_email_ci(
    _db: &ApplicationStore,
    email: &str,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::by_email(&pg, email)).await
}

/// The person who answers to `email`: the row whose primary address it is,
/// or else the row holding it as an alias (LAW-169).
///
/// An alias is a second `person_mailbox` claim, so it is exactly as unique
/// as a primary address and is read by the same direct record read. An
/// attorney who changes firms is found by either address.
///
/// # Errors
///
/// [`PersonError::Db`] if a lookup fails.
pub async fn find_by_any_email(
    db: &ApplicationStore,
    email: &str,
) -> Result<Option<Person>, PersonError> {
    if let Some(person) = find_by_email_ci(db, email).await? {
        return Ok(Some(person));
    }
    match mailbox_holder(db, &email.trim().to_lowercase()).await? {
        Some(id) => find_by_id(db, id).await,
        None => Ok(None),
    }
}

/// Every address `id` holds besides its primary one, lowercased and sorted.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn aliases(_db: &ApplicationStore, id: Uuid) -> Result<Vec<String>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::aliases(&pg, id)).await
}

/// Record `email` as another address of `id`, so an import that names it
/// updates this person instead of creating a second one (LAW-169).
///
/// Idempotent: an address `id` already holds is left as it is. An address
/// another person holds, as primary or alias, is refused, never taken.
///
/// # Errors
///
/// [`PersonError::EmailTaken`] when another person holds `email`;
/// [`PersonError::Db`] if a write fails.
pub async fn add_alias(db: &ApplicationStore, id: Uuid, email: &str) -> Result<(), PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::add_alias(&pg, db, id, email)).await
}

/// The digits of a phone number, minus a leading US `1` on an eleven-digit
/// number, so `+1 (510) 800-2090` and `510.800.2090` compare equal.
fn phone_digits(phone: &str) -> String {
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    match digits.strip_prefix('1') {
        Some(rest) if digits.len() == 11 => rest.to_string(),
        _ => digits,
    }
}

/// The person whose `phone` is the same number as `phone`, ignoring
/// punctuation and a leading US `1`. Phones are stored as typed, so the
/// comparison happens here over the people who have one rather than in an
/// index. `None` for a number with no digits.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn find_by_phone(
    db: &ApplicationStore,
    phone: &str,
) -> Result<Option<Person>, PersonError> {
    if !phone_digits(phone).is_empty() {
        let mut structured = crate::person_phones::person_ids_for_phone(db, phone)
            .await
            .map_err(|error| PersonError::Phone(error.to_string()))?;
        structured.sort_unstable();
        structured.dedup();
        match structured.as_slice() {
            [person_id] => return find_by_id(db, *person_id).await,
            [_first, _second, ..] => return Ok(None),
            [] => {}
        }
    }
    Ok(list_with_phone(db).await?.into_iter().find(|person| {
        person
            .phone
            .as_deref()
            .is_some_and(|p| same_phone(p, phone))
    }))
}

/// Whether two phone numbers name the same line, ignoring punctuation and a
/// leading US `1`. A value with no digits matches nothing.
#[must_use]
pub fn same_phone(left: &str, right: &str) -> bool {
    let wanted = phone_digits(right);
    !wanted.is_empty() && phone_digits(left) == wanted
}

/// Every person who has a phone recorded, so a caller matching many numbers
/// (the inbox list) reads the table once.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn list_with_phone(_db: &ApplicationStore) -> Result<Vec<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::all_with_phone(&pg)).await
}

/// Record, or clear with `None`, the person's direct phone line. Used when a
/// text arrives from a number the firm can now name.
///
/// # Errors
///
/// [`PersonError::Db`] if the write fails.
pub async fn set_phone(
    db: &ApplicationStore,
    id: Uuid,
    phone: Option<&str>,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::set_text(
        &pg, db, id, "phone", phone,
    ))
    .await
}

/// Every person currently holding `role`, in no particular order. The
/// bootstrap-Owner reconciliation uses this to find every `Owner` besides
/// the one the configured `NAVIGATOR_BOOTSTRAP_OWNER_EMAIL` names, so a
/// stale identity from a previous env-var value can be demoted rather than
/// left at `Owner` indefinitely.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn find_by_role(_db: &ApplicationStore, role: Role) -> Result<Vec<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::by_role(&pg, role)).await
}

/// Every person currently holding `role` at one Firm. The PostgreSQL
/// authority path filters through `person_firm_role`; the Surreal fallback
/// reads the deployment-wide role list and narrows it with the Firm's
/// membership rows. [`find_by_role`] has no Firm filter — a digest that
/// emailed every Owner on the deployment would leak across Firms.
///
/// # Errors
///
/// [`PersonError`] on a database failure.
pub async fn find_by_role_and_firm(
    _db: &ApplicationStore,
    role: Role,
    firm_id: Uuid,
) -> Result<Vec<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::by_role_and_firm(
        &pg, role, firm_id,
    ))
    .await
}

/// Resolve a person by their IdP `sub` claim — the first thing the OIDC
/// callback asks, before falling back to the email.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn find_by_oidc_subject(
    db: &ApplicationStore,
    subject: &str,
) -> Result<Option<Person>, PersonError> {
    find_by_subject(db, "oidc_subject", subject).await
}

/// Resolve a person by their Microsoft `sub` claim.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn find_by_microsoft_subject(
    db: &ApplicationStore,
    subject: &str,
) -> Result<Option<Person>, PersonError> {
    find_by_subject(db, "microsoft_subject", subject).await
}

/// Resolve a person by their Apple `sub` claim.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn find_by_apple_subject(
    db: &ApplicationStore,
    subject: &str,
) -> Result<Option<Person>, PersonError> {
    find_by_subject(db, "apple_subject", subject).await
}

async fn find_by_subject(
    _db: &ApplicationStore,
    field: &'static str,
    subject: &str,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::by_subject(&pg, field, subject)).await
}

/// The firm-side person to designate as a matter's lawyer DRI when a create
/// path has no explicit opener — the self-serve intake (no lawyer in the
/// room), the CLI, and Navigator MCP's tool calls. Returns the lowest-id `owner`,
/// else the lowest-id `admin`, else the lowest-id `lawyer` — i.e. the firm
/// principal in a seeded install, resolved by **role**, not a hard-coded
/// email, so a white-label fork gets its own principal with no code
/// change. `None` only on a database with no firm-side person at all
/// (which the caller treats as an error).
///
/// The ladder is ranked here rather than in the query: `IF … THEN … ELSE
/// … END` does not parse inside `ORDER BY`, and the alternative — three
/// ordered queries, or the ladder spelled a second time in SurrealQL —
/// would let [`Role::authority_rank`] and the resolver drift apart.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn default_firm_dri(_db: &ApplicationStore) -> Result<Option<Uuid>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::default_firm_dri(&pg)).await
}

/// Every Person in name order, for a picker or an export that needs the
/// whole directory. A list a person reads page by page goes through
/// [`crate::firms::people_directory`] instead.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn list_directory(_db: &ApplicationStore) -> Result<Vec<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::directory(&pg)).await
}

/// Fuzzy-find people by an optional name and/or email substring. Both
/// needles are matched case-insensitively as substrings and ANDed when
/// both are supplied; the caller is responsible for rejecting the
/// all-`None` case (a blank query would return the whole directory).
/// Results are ordered by name and capped at `limit`.
///
/// This is the read half of the People command boundary: the Navigator MCP
/// `show_person` tool and any web lookup share this one query
/// instead of re-implementing the predicate.
///
/// # Errors
///
/// [`PersonError::Db`] if the lookup fails.
pub async fn search(
    _db: &ApplicationStore,
    name: Option<&str>,
    email: Option<&str>,
    limit: u64,
) -> Result<Vec<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::search(&pg, name, email, limit)).await
}

/// Write a new person row.
///
/// # Errors
///
/// [`PersonError::EmailTaken`] when another row already holds this
/// mailbox case-insensitively, [`PersonError::OidcSubjectTaken`] when
/// another row is already linked to this IdP identity, and
/// [`PersonError::Db`] for anything else.
pub async fn create(db: &ApplicationStore, input: &NewPerson) -> Result<Person, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::create(&pg, db, input)).await
}

/// The person holding this mailbox, creating them if nobody does.
///
/// The canonical seed runs on every boot, so two `web` processes can start
/// together — and `person.role` is the authorization root, so a mailbox
/// that forks into two rows is one human carrying two roles.
///
/// Identity is settled by **claiming the mailbox as its own committed
/// statement** before the person row is written. Nothing here probes for
/// the mailbox first, because that is the shape that forked: every racer
/// minted its own record id and read `email_lower` through a scan, so no
/// two racers touched a key the optimistic layer could conflict on and
/// each committed a row of its own. The claim gives them one identical
/// record key to collide on. See [`CLAIM`] for why it may not be folded
/// back into a transaction with the person write, and
/// `store/tests/person_mailbox_race.rs` for the fork reproduced against
/// the old shape.
///
/// A loser is not refused: it reads the claim's holder by direct record
/// read and returns that person.
///
/// Only the mailbox is matched. A caller that also needs the name or role
/// to be right brings them up to date itself — this settles identity, not
/// content.
///
/// # Errors
///
/// [`PersonError::OidcSubjectTaken`], [`PersonError::MicrosoftSubjectTaken`],
/// or [`PersonError::AppleSubjectTaken`] when `input` carries an IdP identity
/// another person already holds — a real conflict, not a race —
/// [`PersonError::WriteReturnedNothing`] when a claim never resolves into
/// a readable person inside the write budget, and [`PersonError::Db`] for
/// anything else.
pub async fn find_or_create(
    db: &ApplicationStore,
    input: &NewPerson,
) -> Result<Person, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::find_or_create(&pg, db, input)).await
}

/// Edit a person's directory fields. `None` on a field leaves the column
/// alone; a present-but-`None` structured name part clears it. Returns
/// `None` when the person no longer exists.
///
/// This is the persistence half of the People command boundary — the
/// bootstrap-owner guard, the authority-ladder check, and the validation
/// live in [`crate::people_commands`], which calls this once it has
/// decided the edit is allowed.
///
/// `email_lower` is a computed `VALUE string::lowercase(email)` field, so
/// changing the email changes which mailbox this row holds — and the
/// claim that says so has to move with it. A claim left on the old
/// mailbox would lock it out for good, and one never taken on the new
/// mailbox would leave that mailbox forkable. Losing the race for the new
/// mailbox is [`PersonError::EmailTaken`], the same answer the UNIQUE
/// index gives, rather than a database fault.
///
/// # Errors
///
/// [`PersonError::EmailTaken`] when the new email belongs to another
/// row, and [`PersonError::Db`] for anything else.
pub async fn edit(
    db: &ApplicationStore,
    id: Uuid,
    input: &PersonEdit,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::edit(&pg, db, id, input)).await
}

/// Set a person's authority tier. Returns `None` when the person no
/// longer exists.
///
/// # Errors
///
/// [`PersonError::Db`] if the write fails.
pub async fn set_role(
    db: &ApplicationStore,
    id: Uuid,
    role: Role,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::set_role(&pg, db, id, role)).await
}

/// Set (or clear) a person's confirmed-email flag. Returns `None` when the
/// person no longer exists.
///
/// Called from the two places sign-in learns an address is verified: the
/// OIDC callback (`complete_sign_in`, once the IdP's `email_verified` claim
/// is not `false`) and the non-Google confirm-email flow
/// (`email_confirm::confirm`, once Identity Platform reports the address
/// verified). Both calls are best-effort — a failure here must not block
/// sign-in — so this is a plain single-field setter, not routed through
/// [`PersonEdit`]/[`edit`].
///
/// # Errors
///
/// [`PersonError::Db`] if the write fails.
pub async fn set_email_confirmed(
    db: &ApplicationStore,
    id: Uuid,
    confirmed: bool,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::set_flag(
        &pg,
        db,
        id,
        "email_confirmed",
        confirmed,
    ))
    .await
}

/// Set a person's avatar key, returning whether the row existed.
///
/// Surreal validates every field of the row on any update, including fields
/// the query never mentions, so a row still missing either bool field added
/// after person rows already existed — `email_confirmed` or `is_admitted` —
/// fails coercion the moment anything touches it, not only when the write
/// targets that field. Both historical defaults are materialized together in
/// one update, before writing the avatar, so neither is left absent while the
/// other is patched; a deployment-wide backfill stays an explicit operator
/// action.
pub async fn set_profile_image_url(
    db: &ApplicationStore,
    id: Uuid,
    profile_image_url: Option<String>,
) -> Result<bool, PersonError> {
    let pg = postgres_authority().await?;
    Ok(Box::pin(crate::postgres::people::set_text(
        &pg,
        db,
        id,
        "profile_image_url",
        profile_image_url.as_deref(),
    ))
    .await?
    .is_some())
}

/// Read the explicit admission decision for a person.
///
/// The field was added after person rows already existed, so a missing value
/// is the compatible historical shape and means admitted. This is deliberately
/// separate from [`Role`] and project participation: it answers only whether
/// the row may mint a session.
pub async fn is_admitted(_db: &ApplicationStore, id: Uuid) -> Result<bool, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::is_admitted(&pg, id)).await
}

/// Set a person's explicit sign-in admission decision. Returns `None` when
/// the person no longer exists.
///
/// This internal lifecycle flag does not change the person's role or any
/// project participation. It is used when a pending intake is discarded and
/// the retained directory row must no longer be treated as an admission.
pub async fn set_admitted(
    db: &ApplicationStore,
    id: Uuid,
    admitted: bool,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::set_flag(
        &pg,
        db,
        id,
        "is_admitted",
        admitted,
    ))
    .await
}

/// Cache the Xero `ContactID` on a person. No-op (`Ok(None)`) when the
/// person row no longer exists. Idempotent: re-setting the same id just
/// bumps `updated_at`.
///
/// # Errors
///
/// [`PersonError::Db`] if the write fails.
pub async fn set_xero_contact_id(
    db: &ApplicationStore,
    id: Uuid,
    xero_contact_id: &str,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::set_text(
        &pg,
        db,
        id,
        "xero_contact_id",
        Some(xero_contact_id),
    ))
    .await
}

/// Link a person to the IdP identity that just authenticated as them.
/// Returns `None` when the person no longer exists.
///
/// # Errors
///
/// [`PersonError::OidcSubjectTaken`] when another row already holds this
/// `sub`, and [`PersonError::Db`] for anything else.
pub async fn link_oidc_subject(
    db: &ApplicationStore,
    id: Uuid,
    subject: &str,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::link_subject(
        &pg,
        db,
        id,
        "oidc_subject",
        subject,
        false,
    ))
    .await
}

/// Link a person to the Microsoft identity that just authenticated as them.
/// Returns `None` when the person no longer exists.
///
/// # Errors
///
/// [`PersonError::MicrosoftSubjectTaken`] when another row already holds this
/// `sub`, and [`PersonError::Db`] for anything else.
pub async fn link_microsoft_subject(
    db: &ApplicationStore,
    id: Uuid,
    subject: &str,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::link_subject(
        &pg,
        db,
        id,
        "microsoft_subject",
        subject,
        false,
    ))
    .await
}

/// Link a person to the Apple identity that just authenticated as them.
/// Returns `None` when the person no longer exists.
///
/// # Errors
///
/// [`PersonError::AppleSubjectTaken`] when another row already holds this
/// `sub`, and [`PersonError::Db`] for anything else.
pub async fn link_apple_subject(
    db: &ApplicationStore,
    id: Uuid,
    subject: &str,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::link_subject(
        &pg,
        db,
        id,
        "apple_subject",
        subject,
        false,
    ))
    .await
}

/// Move a pre-split identifier out of `oidc_subject` into the Microsoft
/// column. Returns `None` when the person no longer exists.
///
/// See [`adopt_legacy_oidc_subject_as_apple`] for why the move is a single
/// write.
///
/// # Errors
///
/// [`PersonError::MicrosoftSubjectTaken`] when another row already holds this
/// `sub`, and [`PersonError::Db`] for anything else.
pub async fn adopt_legacy_oidc_subject_as_microsoft(
    db: &ApplicationStore,
    id: Uuid,
    subject: &str,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::link_subject(
        &pg,
        db,
        id,
        "microsoft_subject",
        subject,
        true,
    ))
    .await
}

/// Move a pre-split identifier out of `oidc_subject` into the Apple column.
/// Returns `None` when the person no longer exists.
///
/// Rows written before sign-in identifiers were split per provider hold
/// whichever provider authenticated first in `oidc_subject`, with nothing
/// recording which provider issued it. Converging such a row is a *move*, not
/// a copy: the identifier is written to the provider's own column and
/// `oidc_subject` is cleared in the same statement, so the row converges once
/// and stops being a candidate for the resolver's legacy lookup. Doing both in
/// one `UPDATE` is what makes that true — two writes could leave a row holding
/// the same identifier in two columns if the second one failed.
///
/// # Errors
///
/// [`PersonError::AppleSubjectTaken`] when another row already holds this
/// `sub`, and [`PersonError::Db`] for anything else.
pub async fn adopt_legacy_oidc_subject_as_apple(
    db: &ApplicationStore,
    id: Uuid,
    subject: &str,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::link_subject(
        &pg,
        db,
        id,
        "apple_subject",
        subject,
        true,
    ))
    .await
}

/// Apply a directory import's contact facts: the display name and the
/// two free-text contact columns. Deliberately cannot reach `email` or
/// `role` — a re-import is authoritative for how to reach someone, never
/// for who they are or what they may do.
///
/// # Errors
///
/// [`PersonError::Db`] if the write fails.
pub async fn update_contact(
    db: &ApplicationStore,
    id: Uuid,
    input: &ContactUpdate,
) -> Result<Option<Person>, PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::update_contact(&pg, db, id, input)).await
}

/// Remove a person. Idempotent: deleting one that is not there is a
/// no-op.
///
/// Whether a person *may* be deleted — only clients, never the bootstrap
/// Owner — is [`crate::people_commands::delete_person`]'s question, asked
/// before this is called.
///
/// Deleting the row releases its mailbox claims, in that order. A claim
/// that outlived its person would lock that mailbox out of ever being
/// used again — nothing could create the next person to hold it, and
/// [`find_or_create`] would wait out the write budget on a row that is
/// never coming. The release is not best-effort for that reason: a
/// failure to free the mailbox is the caller's to see.
///
/// # Errors
///
/// [`PersonError::Db`] if the delete or the release fails.
pub async fn delete(db: &ApplicationStore, id: Uuid) -> Result<(), PersonError> {
    let pg = postgres_authority().await?;
    Box::pin(crate::postgres::people::delete(&pg, db, id)).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn phone_digits_ignores_punctuation_and_a_leading_us_one() {
        assert_eq!(super::phone_digits("+1 (510) 800-2090"), "5108002090");
        assert_eq!(super::phone_digits("510.800.2090"), "5108002090");
        assert_eq!(super::phone_digits("+44 20 7946 0958"), "442079460958");
        assert_eq!(super::phone_digits("none"), "");
    }

    use super::Role;

    #[test]
    fn lawyer_and_clerk_tiers_are_disjoint() {
        assert!(Role::Owner.is_lawyer_tier());
        assert!(Role::Owner.is_admin_tier());
        assert!(Role::Owner.is_owner());
        assert_eq!(Role::Owner.authority_rank(), 4);
        assert!(Role::Admin.is_admin_tier());
        assert!(!Role::Admin.is_owner());
        assert!(Role::Lawyer.is_lawyer_tier());
        assert!(Role::Admin.is_lawyer_tier());
        assert!(!Role::Clerk.is_lawyer_tier());
        assert!(Role::Clerk.is_clerk());
        assert!(!Role::Client.is_clerk());
    }

    #[test]
    fn every_role_round_trips_through_its_stored_spelling() {
        // The stored spelling is what the schema's `ASSERT $value IN
        // [...]` names and what `PersonRow` reads back, so a role that
        // did not round-trip would be a row this module wrote and then
        // could not load.
        for role in [
            Role::Owner,
            Role::Admin,
            Role::Lawyer,
            Role::Clerk,
            Role::Client,
        ] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
        assert_eq!(Role::parse("wizard"), None);
        assert_eq!(Role::default(), Role::Client);
    }

    // The current CREATE projection references both fields. Insert the row
    // directly into the historical schema before restoring its definitions.
}
