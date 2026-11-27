//! Recorded delegations: the fact that one client helps another with
//! their matters.
//!
//! A mailbox is how we reach you. It is not who you are, and it is not
//! permission to be someone else. `person.email` is required and
//! `person_email_lower` is UNIQUE, so today the address on a Person row
//! doubles as that Person's login identifier — which means a client with
//! no email address cannot be reached by the portal, and sharing a
//! relative's address would make one credential resolve to whichever row
//! it happened to match. This module is the deliberate alternative: the
//! link is written by a named firm member, on a stated basis, and it can
//! be revoked and audited.
//!
//! # This module grants nothing
//!
//! A [`Delegation`] records a relationship. It confers no read, no write,
//! and no session. No authorization path reads this table, the same
//! posture [`crate::external_identities`] holds, and
//! `store/tests/person_delegate_is_inert.rs` pins that against the
//! authorization surfaces by name.
//!
//! What a delegate may *do* is a separate decision that has not been
//! made. [`crate::access::MatterViewer::ClientDri`] carries plan
//! approval, so a session acting through a delegation must never reach it
//! without its own explicit, separately recorded grant. That is why there
//! is no `may_act` column: an unread permission flag is what gets wired
//! up carelessly later. [`Basis`] is the legal shape of the
//! relationship, not a capability.
//!
//! # Both sides must be clients
//!
//! A delegation is confined to [`Role::Client`] on both sides. Firm-side
//! people get exactly one profile: themselves. Without that confinement
//! the mechanism would let a clerk act as an admin, or one lawyer act as
//! another — privilege escalation across the firm dressed up as a
//! convenience for clients.
//!
//! The confinement is enforced twice, and the second time is the one that
//! matters. [`grant`] refuses a non-client on either side. But
//! `person.role` is written through three doors —
//! [`crate::persons::set_role`], [`crate::persons::edit`], and
//! [`crate::persons::create`] — so a cascade hooked to any one of them
//! would look like the control without being it. Instead
//! [`live_for_delegate`] and [`live_for_subject`] re-read both roles at
//! *read* time. That cannot be bypassed by a role-change door which does
//! not exist yet, nor by a direct database write.
//!
//! A grant whose parties are no longer both clients is **dormant**, which
//! is deliberately distinct from **revoked**: [`Delegation::state`]
//! reports which, so a surface can say "dormant because this person is
//! now firm-side" rather than silently hiding the row. A dormant grant
//! becomes live again if both parties are clients again; ending a
//! delegation for good is [`revoke`], which is a recorded act with a named
//! actor.
//!
//! A client's helper who works at the firm therefore cannot be linked.
//! That is correct rather than a limitation — it is a conflicts question
//! for a lawyer, not a portal feature.
//!
//! # The same defect exists at the other membership door
//!
//! [`crate::firms::add_membership`] answers the mirror-image question:
//! it refuses a `client`-tier person a `person_firm_role` row with
//! [`crate::firms::FirmError::ClientCannotJoinFirm`], because a client
//! reaches a matter through `person_project_role` and never through firm
//! membership. Both are the same class — `person.role` decided at a
//! membership write path — approached from opposite sides: that one keeps
//! a client out of the firm, this one keeps the firm out of a client's
//! profile.
//!
//! The two are deliberately shaped alike, so they can later fold into one
//! shared "may this person hold this kind of membership" predicate without
//! either having to change its contract: a typed error variant naming the
//! person, refused at the write path before any row is created, with a
//! test that walks every tier rather than sampling one. The one difference
//! is that `ClientCannotJoinFirm` has a single disqualifying role and can
//! name it in the message, whereas a delegation has four, so
//! [`DelegationError::DelegateNotClient`] and
//! [`DelegationError::SubjectNotClient`] carry the offending role in the
//! payload.

use serde::Serialize;
use uuid::Uuid;

use sqlx::{postgres::PgRow, AssertSqlSafe, Row};

use crate::application_store::ApplicationStore;
use crate::persons::Role;
use crate::postgres::identity::{self as authority, AuthorityError};
use crate::postgres::PostgresDb;

const TABLE: &str = "person_delegate";
const SELECT: &str = "id, delegate_person_id, subject_person_id, basis, \
                      instrument_reference, granted_by_person_id, granted_at, \
                      subject_notified_at, subject_notified_channel, \
                      revoked_at, revoked_by_person_id, inserted_at, updated_at";

/// The legal shape of a delegation — why one person may help another.
///
/// Not a capability. Each variant carries a different revocation trigger
/// in the world (a power of attorney ends on its own terms, a guardianship
/// on a court order, an assistant designation on a phone call), so a
/// boolean cannot hold them. Matches the `ASSERT` on
/// `person_delegate.basis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Basis {
    /// A helper with no legal instrument behind them — the spouse who
    /// reads the portal on behalf of a client who has no email address of
    /// their own. The narrowest basis, and the only one that asserts
    /// nothing about a document.
    Assistant,
    /// A power of attorney. Asserts a valid instrument exists, so
    /// [`NewDelegation::instrument_reference`] should name it.
    PowerOfAttorney,
    /// A court-appointed guardian.
    Guardian,
    /// The personal representative of an estate.
    Executor,
    /// A trustee acting for a trust's beneficiary.
    Trustee,
    /// A parent or legal guardian of a minor client.
    ParentOfMinor,
}

impl Basis {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assistant => "assistant",
            Self::PowerOfAttorney => "power_of_attorney",
            Self::Guardian => "guardian",
            Self::Executor => "executor",
            Self::Trustee => "trustee",
            Self::ParentOfMinor => "parent_of_minor",
        }
    }

    /// The basis named by its stored spelling, or `None` for anything
    /// else. The inverse of [`Basis::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "assistant" => Some(Self::Assistant),
            "power_of_attorney" => Some(Self::PowerOfAttorney),
            "guardian" => Some(Self::Guardian),
            "executor" => Some(Self::Executor),
            "trustee" => Some(Self::Trustee),
            "parent_of_minor" => Some(Self::ParentOfMinor),
            _ => None,
        }
    }

    /// Whether this basis claims a legal instrument stands behind it.
    ///
    /// [`Basis::Assistant`] does not; every other variant does, and a row
    /// carrying one without an
    /// [`instrument_reference`](NewDelegation::instrument_reference) is
    /// asserting a legal fact about a client that the firm has not
    /// recorded reading. [`grant`] refuses that.
    #[must_use]
    pub fn claims_an_instrument(self) -> bool {
        !matches!(self, Self::Assistant)
    }
}

/// How the subject was told their delegation exists.
///
/// Recorded rather than assumed, because a subject with no mailbox cannot
/// be notified by email, and a grant whose only notice channel is the
/// *grantee's* mailbox leaves the subject structurally unable to learn
/// about their own grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum NoticeChannel {
    InPerson,
    Telephone,
    Post,
    Email,
}

impl NoticeChannel {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InPerson => "in_person",
            Self::Telephone => "telephone",
            Self::Post => "post",
            Self::Email => "email",
        }
    }

    /// The channel named by its stored spelling, or `None` for anything
    /// else.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "in_person" => Some(Self::InPerson),
            "telephone" => Some(Self::Telephone),
            "post" => Some(Self::Post),
            "email" => Some(Self::Email),
            _ => None,
        }
    }
}

/// Whether a recorded delegation is currently in force.
///
/// Three states rather than two, because "withdrawn by the firm" and "not
/// currently permissible" are different facts and a surface should be able
/// to say which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DelegationState {
    /// In force: not revoked, and both parties are still clients.
    Live,
    /// Withdrawn on a recorded date by a recorded actor. Terminal — a
    /// revoked row is never re-activated; a new [`grant`] writes a new row.
    Revoked,
    /// Not revoked, but at least one party is no longer
    /// [`Role::Client`], so the delegation may not be acted on. Becomes
    /// [`DelegationState::Live`] again if both are clients again.
    Dormant,
}

/// One recorded delegation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Delegation {
    pub id: Uuid,
    /// The client who may help.
    pub delegate_person_id: Uuid,
    /// The client being helped.
    pub subject_person_id: Uuid,
    pub basis: Basis,
    /// The document a non-[`Basis::Assistant`] basis rests on, as the
    /// attorney recorded it.
    pub instrument_reference: Option<String>,
    /// The firm member who wrote this row.
    pub granted_by_person_id: Uuid,
    pub granted_at: String,
    /// When the subject was told, if they have been.
    pub subject_notified_at: Option<String>,
    pub subject_notified_channel: Option<NoticeChannel>,
    pub revoked_at: Option<String>,
    pub revoked_by_person_id: Option<Uuid>,
    pub inserted_at: String,
    pub updated_at: String,
}

impl Delegation {
    /// Whether this row has been withdrawn.
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// Whether the subject has been told this delegation exists.
    ///
    /// A live delegation the subject has not been told about is not a bug
    /// in this module — the firm may write the row before the telephone
    /// call — but it is a thing a surface should be able to surface.
    #[must_use]
    pub fn subject_was_notified(&self) -> bool {
        self.subject_notified_at.is_some()
    }

    /// This row's state given both parties' current roles.
    ///
    /// Revocation wins over dormancy: a withdrawn delegation stays
    /// withdrawn whatever the roles later become.
    #[must_use]
    pub fn state(&self, delegate_role: Role, subject_role: Role) -> DelegationState {
        if self.is_revoked() {
            DelegationState::Revoked
        } else if delegate_role == Role::Client && subject_role == Role::Client {
            DelegationState::Live
        } else {
            DelegationState::Dormant
        }
    }
}

/// The fields a new delegation needs.
#[derive(Debug, Clone)]
pub struct NewDelegation {
    /// The client who may help. Must be [`Role::Client`].
    pub delegate_person_id: Uuid,
    /// The client being helped. Must be [`Role::Client`], and must not be
    /// the delegate.
    pub subject_person_id: Uuid,
    pub basis: Basis,
    /// Required for every basis except [`Basis::Assistant`]: a row
    /// claiming a power of attorney asserts a legal fact about a client,
    /// and the firm should name what it read before asserting it.
    pub instrument_reference: Option<String>,
    /// The firm member recording this. Not required to be a client — this
    /// is the one person id on the row that is expected to be firm-side.
    pub granted_by_person_id: Uuid,
    /// When and how the subject was told, if they have been already.
    pub subject_notified_at: Option<String>,
    pub subject_notified_channel: Option<NoticeChannel>,
}

/// Errors from the delegation seam.
#[derive(Debug, thiserror::Error)]
pub enum DelegationError {
    #[error(transparent)]
    Person(#[from] crate::persons::PersonError),
    #[error("writing a delegation returned no usable row")]
    WriteReturnedNothing,
    #[error("no person {0}")]
    NoSuchPerson(Uuid),
    #[error("no delegation {0}")]
    NoSuchDelegation(Uuid),
    /// The delegate is not a client. Firm-side people get exactly one
    /// profile: themselves.
    #[error("person {0} is a {1} and only a client may be a delegate")]
    DelegateNotClient(Uuid, &'static str),
    /// The subject is not a client. Same rule, other side — a delegation
    /// must never be a route into a firm-side account.
    #[error("person {0} is a {1} and only a client may be a delegation subject")]
    SubjectNotClient(Uuid, &'static str),
    #[error("a person cannot be their own delegate")]
    SelfDelegation,
    #[error("basis {0} asserts a legal instrument, so instrument_reference is required")]
    InstrumentReferenceRequired(&'static str),
    #[error("that pair already has a live delegation")]
    AlreadyGranted,
    #[error("delegation {0} is already revoked")]
    AlreadyRevoked(Uuid),
    /// PostgreSQL identity authority is configured and failed.
    #[error(transparent)]
    Authority(#[from] AuthorityError),
}

fn rfc3339(row: &PgRow, column: &str) -> Result<Option<String>, sqlx::Error> {
    Ok(row
        .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(column)?
        .map(|at| at.to_rfc3339()))
}

fn from_pg(row: &PgRow) -> Result<Option<Delegation>, sqlx::Error> {
    let basis: String = row.try_get("basis")?;
    let Some(basis) = Basis::parse(&basis) else {
        return Ok(None);
    };
    let channel: Option<String> = row.try_get("subject_notified_channel")?;
    Ok(Some(Delegation {
        id: row.try_get("id")?,
        delegate_person_id: row.try_get("delegate_person_id")?,
        subject_person_id: row.try_get("subject_person_id")?,
        basis,
        instrument_reference: row.try_get("instrument_reference")?,
        granted_by_person_id: row.try_get("granted_by_person_id")?,
        granted_at: rfc3339(row, "granted_at")?.unwrap_or_default(),
        subject_notified_at: rfc3339(row, "subject_notified_at")?,
        subject_notified_channel: channel.as_deref().and_then(NoticeChannel::parse),
        revoked_at: rfc3339(row, "revoked_at")?,
        revoked_by_person_id: row.try_get("revoked_by_person_id")?,
        inserted_at: rfc3339(row, "inserted_at")?.unwrap_or_default(),
        updated_at: rfc3339(row, "updated_at")?.unwrap_or_default(),
    }))
}

/// Delegations from PostgreSQL matching `predicate` over the bound UUIDs,
/// oldest grant first.
async fn pg_rows(
    pg: &PostgresDb,
    predicate: &str,
    ids: &[Uuid],
) -> Result<Vec<Delegation>, DelegationError> {
    let mut query = sqlx::query(AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE {predicate} ORDER BY granted_at, id"
    )));
    for id in ids {
        query = query.bind(*id);
    }
    let rows = query.fetch_all(&pg.0).await.map_err(AuthorityError::from)?;
    let rows: Vec<Option<Delegation>> = rows
        .iter()
        .map(from_pg)
        .collect::<Result<_, _>>()
        .map_err(AuthorityError::from)?;
    Ok(rows.into_iter().flatten().collect())
}

/// Run one delegation write in PostgreSQL and project the row it returns.
async fn pg_write(
    pg: &PostgresDb,
    query: sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
) -> Result<Delegation, DelegationError> {
    let row = query
        .fetch_optional(&pg.0)
        .await
        .map_err(AuthorityError::from)?
        .ok_or(DelegationError::WriteReturnedNothing)?;
    let delegation = from_pg(&row)
        .map_err(AuthorityError::from)?
        .ok_or(DelegationError::WriteReturnedNothing)?;

    Ok(delegation)
}

/// Delegation accessors operate only on admitted PostgreSQL authority.
async fn required_authority() -> Result<PostgresDb, DelegationError> {
    Ok(authority::authority().await?)
}

/// The role of a person who must exist, or [`DelegationError::NoSuchPerson`].
async fn role_of(application_store: &ApplicationStore, id: Uuid) -> Result<Role, DelegationError> {
    crate::persons::find_by_id(application_store, id)
        .await?
        .map(|person| person.role)
        .ok_or(DelegationError::NoSuchPerson(id))
}

/// Record that one client may help another.
///
/// Refuses a non-client on either side, self-delegation, a basis that
/// claims an instrument without naming one, and a second live delegation
/// for a pair that already has one.
///
/// The pair index is deliberately not `UNIQUE` — a revoked row stays on
/// the table so a pair may hold several rows over time — so the live-grant
/// refusal is a read-back rather than an index violation, the same
/// convention `authority_use` documents. Two concurrent grants for one
/// pair can therefore both pass the check and land two live rows. That is
/// a duplicate of the same permission rather than an escalation, and
/// [`live_for_delegate`] returns both; it is not worth a lock.
///
/// # Errors
///
/// See [`DelegationError`]. Notably [`DelegationError::DelegateNotClient`]
/// and [`DelegationError::SubjectNotClient`] — the write-path half of the
/// client-only confinement, whose read-path half is in
/// [`live_for_delegate`].
pub async fn grant(
    application_store: &ApplicationStore,
    input: &NewDelegation,
) -> Result<Delegation, DelegationError> {
    if input.delegate_person_id == input.subject_person_id {
        return Err(DelegationError::SelfDelegation);
    }
    if input.basis.claims_an_instrument()
        && input
            .instrument_reference
            .as_ref()
            .is_none_or(|reference| reference.trim().is_empty())
    {
        return Err(DelegationError::InstrumentReferenceRequired(
            input.basis.as_str(),
        ));
    }

    let pg = required_authority().await?;
    let delegate_role = role_of(application_store, input.delegate_person_id).await?;
    if delegate_role != Role::Client {
        return Err(DelegationError::DelegateNotClient(
            input.delegate_person_id,
            delegate_role.as_str(),
        ));
    }
    let subject_role = role_of(application_store, input.subject_person_id).await?;
    if subject_role != Role::Client {
        return Err(DelegationError::SubjectNotClient(
            input.subject_person_id,
            subject_role.as_str(),
        ));
    }
    if crate::persons::find_by_id(application_store, input.granted_by_person_id)
        .await?
        .is_none()
    {
        return Err(DelegationError::NoSuchPerson(input.granted_by_person_id));
    }
    if !find_live_for_pair(
        application_store,
        input.delegate_person_id,
        input.subject_person_id,
    )
    .await?
    .is_empty()
    {
        return Err(DelegationError::AlreadyGranted);
    }

    let sql = format!(
        "INSERT INTO {TABLE} (id, delegate_person_id, subject_person_id, basis, \
             instrument_reference, granted_by_person_id, granted_at, subject_notified_at, \
             subject_notified_channel) \
             VALUES ($1, $2, $3, $4, $5, $6, now(), $7::timestamptz, $8) RETURNING {SELECT}"
    );
    let query = sqlx::query(AssertSqlSafe(sql))
        .bind(Uuid::now_v7())
        .bind(input.delegate_person_id)
        .bind(input.subject_person_id)
        .bind(input.basis.as_str())
        .bind(input.instrument_reference.clone())
        .bind(input.granted_by_person_id)
        .bind(input.subject_notified_at.clone())
        .bind(input.subject_notified_channel.map(NoticeChannel::as_str));
    Box::pin(pg_write(&pg, query)).await
}

/// Withdraw a delegation, naming who withdrew it and when.
///
/// Never a delete: "this link existed and was withdrawn on this date" is
/// the fact a later inquiry needs, and a deleted row answers nothing —
/// the same choice `person.is_admitted` makes. Revocation is terminal;
/// re-granting writes a new row.
///
/// `revoked_by_person_id` is not required to be a client: withdrawing is
/// expected to be a firm-side act, and — per the Legal Council — must not
/// require the credential holder.
///
/// # Errors
///
/// [`DelegationError::NoSuchDelegation`] if the row is gone,
/// [`DelegationError::AlreadyRevoked`] if it is already withdrawn.
pub async fn revoke(
    application_store: &ApplicationStore,
    id: Uuid,
    revoked_by_person_id: Uuid,
) -> Result<Delegation, DelegationError> {
    let existing = find_by_id(application_store, id)
        .await?
        .ok_or(DelegationError::NoSuchDelegation(id))?;
    if existing.is_revoked() {
        return Err(DelegationError::AlreadyRevoked(id));
    }
    if crate::persons::find_by_id(application_store, revoked_by_person_id)
        .await?
        .is_none()
    {
        return Err(DelegationError::NoSuchPerson(revoked_by_person_id));
    }
    let pg = required_authority().await?;
    let sql = format!(
        "UPDATE {TABLE} SET revoked_at = now(), revoked_by_person_id = $2, \
             updated_at = now() WHERE id = $1 RETURNING {SELECT}"
    );
    let query = sqlx::query(AssertSqlSafe(sql))
        .bind(id)
        .bind(revoked_by_person_id);
    Box::pin(pg_write(&pg, query)).await
}

/// Record that the subject was told about a delegation, and how.
///
/// Separate from [`grant`] because the telephone call may happen after the
/// row is written, and separate from consent because being told and
/// agreeing are different facts.
///
/// # Errors
///
/// [`DelegationError::NoSuchDelegation`] if the row is gone.
pub async fn record_subject_notice(
    application_store: &ApplicationStore,
    id: Uuid,
    channel: NoticeChannel,
) -> Result<Delegation, DelegationError> {
    if find_by_id(application_store, id).await?.is_none() {
        return Err(DelegationError::NoSuchDelegation(id));
    }
    let pg = required_authority().await?;
    let sql = format!(
        "UPDATE {TABLE} SET subject_notified_at = now(), subject_notified_channel = $2, \
             updated_at = now() WHERE id = $1 RETURNING {SELECT}"
    );
    let query = sqlx::query(AssertSqlSafe(sql))
        .bind(id)
        .bind(channel.as_str());
    Box::pin(pg_write(&pg, query)).await
}

/// One delegation by id, whatever its state.
///
/// # Errors
///
/// [`DelegationError::Authority`] if the read fails.
pub async fn find_by_id(
    _application_store: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Delegation>, DelegationError> {
    let pg = required_authority().await?;
    Ok(Box::pin(pg_rows(&pg, "id = $1", &[id]))
        .await?
        .into_iter()
        .next())
}

/// Every delegation row naming this person as the delegate, revoked and
/// dormant ones included. The administrative view.
///
/// # Errors
///
/// [`DelegationError::Authority`] if the read fails.
pub async fn all_for_delegate(
    application_store: &ApplicationStore,
    delegate_person_id: Uuid,
) -> Result<Vec<Delegation>, DelegationError> {
    rows_where(
        application_store,
        "delegate_person_id = $person",
        delegate_person_id,
    )
    .await
}

/// Every delegation row naming this person as the subject, revoked and
/// dormant ones included.
///
/// This is the answer to "who can act as me?", and it must stay
/// answerable without the subject's own credential — a client with no
/// mailbox cannot self-serve, so the firm reads this on their behalf.
///
/// # Errors
///
/// [`DelegationError::Authority`] if the read fails.
pub async fn all_for_subject(
    application_store: &ApplicationStore,
    subject_person_id: Uuid,
) -> Result<Vec<Delegation>, DelegationError> {
    rows_where(
        application_store,
        "subject_person_id = $person",
        subject_person_id,
    )
    .await
}

async fn rows_where(
    _application_store: &ApplicationStore,
    predicate: &str,
    person_id: Uuid,
) -> Result<Vec<Delegation>, DelegationError> {
    let pg = required_authority().await?;
    let predicate = predicate.replace("$person", "$1");
    Box::pin(pg_rows(&pg, &predicate, &[person_id])).await
}

/// The delegations this person may currently act through.
///
/// This is the read-path half of the client-only confinement, and the half
/// that actually holds: it drops any row whose delegate or subject is no
/// longer [`Role::Client`], whatever wrote that role change. Because
/// `person.role` is written through [`crate::persons::set_role`],
/// [`crate::persons::edit`] *and* [`crate::persons::create`], a cascade
/// hooked to one of them would cover a third of the doors while looking
/// like the control. Re-reading the roles here cannot be bypassed by a
/// door that does not exist yet, nor by a direct database write.
///
/// Revoked rows are dropped too. Use [`all_for_delegate`] for the
/// administrative view that keeps them.
///
/// # Errors
///
/// [`DelegationError::Authority`] if a read fails.
pub async fn live_for_delegate(
    application_store: &ApplicationStore,
    delegate_person_id: Uuid,
) -> Result<Vec<Delegation>, DelegationError> {
    let _pg = required_authority().await?;
    let delegate_role = role_of(application_store, delegate_person_id).await?;
    if delegate_role != Role::Client {
        return Ok(Vec::new());
    }
    retain_live(
        application_store,
        all_for_delegate(application_store, delegate_person_id).await?,
    )
    .await
}

/// The delegations currently in force over this person — who may act as
/// them right now.
///
/// The same read-time re-validation [`live_for_delegate`] performs.
///
/// # Errors
///
/// [`DelegationError::Authority`] if a read fails.
pub async fn live_for_subject(
    application_store: &ApplicationStore,
    subject_person_id: Uuid,
) -> Result<Vec<Delegation>, DelegationError> {
    let _pg = required_authority().await?;
    let subject_role = role_of(application_store, subject_person_id).await?;
    if subject_role != Role::Client {
        return Ok(Vec::new());
    }
    retain_live(
        application_store,
        all_for_subject(application_store, subject_person_id).await?,
    )
    .await
}

/// Drop every row that is revoked or whose counterparties are not both
/// clients. Roles are read per row rather than cached: the set is small,
/// and a stale role here is the exact hole this function exists to close.
///
/// A missing counterparty is not live and does not hide unrelated rows.
async fn retain_live(
    application_store: &ApplicationStore,
    rows: Vec<Delegation>,
) -> Result<Vec<Delegation>, DelegationError> {
    let mut live = Vec::new();
    for row in rows {
        if row.is_revoked() {
            continue;
        }
        let (Some(delegate_role), Some(subject_role)) = (
            optional_role_of(application_store, row.delegate_person_id).await?,
            optional_role_of(application_store, row.subject_person_id).await?,
        ) else {
            continue;
        };
        if row.state(delegate_role, subject_role) == DelegationState::Live {
            live.push(row);
        }
    }
    Ok(live)
}

/// The role of a person who may or may not still exist.
async fn optional_role_of(
    application_store: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Role>, DelegationError> {
    Ok(crate::persons::find_by_id(application_store, id)
        .await?
        .map(|person| person.role))
}

/// The live delegations for one exact pair. Used by [`grant`] to refuse a
/// duplicate, and normally holding zero or one row.
///
/// # Errors
///
/// [`DelegationError::Authority`] if a read fails.
pub async fn find_live_for_pair(
    _application_store: &ApplicationStore,
    delegate_person_id: Uuid,
    subject_person_id: Uuid,
) -> Result<Vec<Delegation>, DelegationError> {
    let pg = required_authority().await?;
    Box::pin(pg_rows(
        &pg,
        "delegate_person_id = $1 AND subject_person_id = $2 AND revoked_at IS NULL",
        &[delegate_person_id, subject_person_id],
    ))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persons::{self, NewPerson};

    /// A client, the ordinary case — `NewPerson` defaults to [`Role::Client`].
    async fn client(db: &ApplicationStore, name: &str, email: &str) -> Uuid {
        persons::create(db, &NewPerson::new(name, email))
            .await
            .unwrap()
            .id
    }

    async fn person_at(db: &ApplicationStore, name: &str, email: &str, role: Role) -> Uuid {
        persons::create(db, &NewPerson::with_role(name, email, role))
            .await
            .unwrap()
            .id
    }

    /// The motivating shape: two clients on one matter, one of whom cannot
    /// receive email, and a firm member recording the arrangement.
    struct Household {
        delegate: Uuid,
        subject: Uuid,
        firm: Uuid,
    }

    async fn household(db: &ApplicationStore) -> Household {
        Household {
            delegate: client(db, "Delegate Client", "delegate@example.com").await,
            subject: client(db, "Subject Client", "subject@example.com").await,
            firm: person_at(db, "Firm Lawyer", "lawyer@example.com", Role::Lawyer).await,
        }
    }

    /// Move a person to another tier through [`persons::set_role`], the
    /// first of the three doors onto the role column. Asserts the row
    /// survived, so the returned `Option` is consumed rather than dropped.
    async fn set_role_to(db: &ApplicationStore, id: Uuid, role: Role) {
        persons::set_role(db, id, role)
            .await
            .unwrap()
            .expect("the person must still exist after a role change");
    }

    /// The same move through [`persons::edit`], the second door.
    async fn edit_role_to(db: &ApplicationStore, id: Uuid, role: Role) {
        persons::edit(
            db,
            id,
            &persons::PersonEdit {
                role: Some(role),
                ..persons::PersonEdit::default()
            },
        )
        .await
        .unwrap()
        .expect("the person must still exist after an edit");
    }

    fn assistant(h: &Household) -> NewDelegation {
        NewDelegation {
            delegate_person_id: h.delegate,
            subject_person_id: h.subject,
            basis: Basis::Assistant,
            instrument_reference: None,
            granted_by_person_id: h.firm,
            subject_notified_at: None,
            subject_notified_channel: None,
        }
    }

    #[tokio::test]
    async fn grant_round_trips_and_names_who_recorded_it() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;

            let granted = grant(&db, &assistant(&h)).await.unwrap();

            assert_eq!(granted.delegate_person_id, h.delegate);
            assert_eq!(granted.subject_person_id, h.subject);
            assert_eq!(granted.basis, Basis::Assistant);
            assert_eq!(
                granted.granted_by_person_id, h.firm,
                "the row must name the firm member who wrote it",
            );
            assert!(!granted.is_revoked());
            assert!(
                !granted.subject_was_notified(),
                "a grant does not itself notify the subject",
            );

            let reloaded = find_by_id(&db, granted.id).await.unwrap().unwrap();
            assert_eq!(reloaded, granted);
        })
        .await;
    }

    // --- Nick's constraint: client on both sides, refused at the write path.

    #[tokio::test]
    async fn a_non_client_delegate_is_refused_for_every_firm_tier() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            for role in [Role::Owner, Role::Admin, Role::Lawyer, Role::Clerk] {
                let firm_side = person_at(
                    &db,
                    "Firm Side",
                    &format!("firm-{}@example.com", role.as_str()),
                    role,
                )
                .await;

                let error = grant(
                    &db,
                    &NewDelegation {
                        delegate_person_id: firm_side,
                        ..assistant(&h)
                    },
                )
                .await
                .expect_err("a firm-side person must not be a delegate");

                assert!(
                    matches!(error, DelegationError::DelegateNotClient(id, named)
                    if id == firm_side && named == role.as_str()),
                    "expected DelegateNotClient for {}, got {error:?}",
                    role.as_str(),
                );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn a_non_client_subject_is_refused_for_every_firm_tier() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            for role in [Role::Owner, Role::Admin, Role::Lawyer, Role::Clerk] {
                let firm_side = person_at(
                    &db,
                    "Firm Side",
                    &format!("firm-{}@example.com", role.as_str()),
                    role,
                )
                .await;

                let error = grant(
                    &db,
                    &NewDelegation {
                        subject_person_id: firm_side,
                        ..assistant(&h)
                    },
                )
                .await
                .expect_err("a firm-side person must not be a delegation subject");

                assert!(
                    matches!(error, DelegationError::SubjectNotClient(id, named)
                    if id == firm_side && named == role.as_str()),
                    "expected SubjectNotClient for {}, got {error:?}",
                    role.as_str(),
                );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn the_firm_member_recording_a_grant_need_not_be_a_client() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            // `h.firm` is a Lawyer, and it is the `granted_by` on every grant in
            // these tests — so this passing is the assertion that the client-only
            // rule binds the two linked sides and not the recorder.
            grant(&db, &assistant(&h))
                .await
                .expect("a lawyer must be able to record a delegation between clients");
        })
        .await;
    }

    // --- Role changes. The behaviour the design chose deliberately.

    #[tokio::test]
    async fn promoting_a_party_makes_the_grant_dormant_not_live() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let granted = grant(&db, &assistant(&h)).await.unwrap();

            assert_eq!(
                live_for_delegate(&db, h.delegate).await.unwrap(),
                vec![granted.clone()],
            );

            // The hole Nick's constraint closes: a linked client is hired by the
            // firm. No cascade runs — `set_role` knows nothing about this table —
            // so the read path is what must refuse.
            set_role_to(&db, h.delegate, Role::Clerk).await;

            assert!(
                live_for_delegate(&db, h.delegate).await.unwrap().is_empty(),
                "a clerk must not act through a delegation",
            );
            assert!(
                live_for_subject(&db, h.subject).await.unwrap().is_empty(),
                "and the subject side must agree",
            );

            // Dormant, not revoked — the distinction a surface needs in order to
            // say *why* the row is not in force.
            let reloaded = find_by_id(&db, granted.id).await.unwrap().unwrap();
            assert!(!reloaded.is_revoked(), "dormancy is not revocation");
            assert_eq!(
                reloaded.state(Role::Clerk, Role::Client),
                DelegationState::Dormant,
            );
            assert_eq!(
                all_for_delegate(&db, h.delegate).await.unwrap(),
                vec![reloaded],
                "the administrative view still shows a dormant row",
            );
        })
        .await;
    }

    #[tokio::test]
    async fn promoting_the_subject_alone_also_makes_the_grant_dormant() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            grant(&db, &assistant(&h)).await.unwrap();

            set_role_to(&db, h.subject, Role::Lawyer).await;

            assert!(
                live_for_delegate(&db, h.delegate).await.unwrap().is_empty(),
                "a delegation must never be a route into a firm-side account",
            );
        })
        .await;
    }

    /// The deliberate, debatable half of the role-change decision: because
    /// liveness is recomputed rather than cascaded, returning a party to
    /// `client` returns the grant to force without anyone re-authorizing it.
    ///
    /// Pinned so the choice is visible. A reviewer who wants re-authorization
    /// instead changes this test and adds an explicit revoke on role change.
    #[tokio::test]
    async fn returning_a_party_to_client_makes_a_dormant_grant_live_again() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let granted = grant(&db, &assistant(&h)).await.unwrap();

            set_role_to(&db, h.delegate, Role::Clerk).await;
            assert!(live_for_delegate(&db, h.delegate).await.unwrap().is_empty());

            set_role_to(&db, h.delegate, Role::Client).await;
            assert_eq!(
                live_for_delegate(&db, h.delegate).await.unwrap(),
                vec![granted],
                "dormancy tracks the current roles; ending a delegation for good \
             is `revoke`, which is a recorded act",
            );
        })
        .await;
    }

    /// `persons::edit` is a second door onto the role column, and
    /// `persons::create` is a third. A cascade hooked to `set_role` would
    /// cover one of the three; read-time re-validation covers all of them.
    #[tokio::test]
    async fn a_role_change_through_the_edit_door_is_also_caught() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            grant(&db, &assistant(&h)).await.unwrap();

            edit_role_to(&db, h.delegate, Role::Admin).await;

            assert!(
                live_for_delegate(&db, h.delegate).await.unwrap().is_empty(),
                "the read path must not care which door changed the role",
            );
        })
        .await;
    }

    // --- Revocation.

    #[tokio::test]
    async fn revoke_records_who_and_when_and_never_deletes() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let granted = grant(&db, &assistant(&h)).await.unwrap();

            let revoked = revoke(&db, granted.id, h.firm).await.unwrap();

            assert!(revoked.is_revoked());
            assert_eq!(revoked.revoked_by_person_id, Some(h.firm));
            assert!(revoked.revoked_at.is_some());
            assert!(
                live_for_delegate(&db, h.delegate).await.unwrap().is_empty(),
                "a revoked delegation is not in force",
            );
            assert!(
                find_by_id(&db, granted.id).await.unwrap().is_some(),
                "the row survives revocation, because `this link existed and was \
             withdrawn on this date` is the fact an inquiry needs",
            );
        })
        .await;
    }

    #[tokio::test]
    async fn revocation_does_not_require_the_credential_holder() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let granted = grant(&db, &assistant(&h)).await.unwrap();

            // `h.firm` is neither the delegate nor the subject. The firm can end a
            // delegation without the person holding the login cooperating, which
            // is the control that protects a client whose credential is held by
            // someone else.
            revoke(&db, granted.id, h.firm)
                .await
                .expect("the firm must be able to revoke unilaterally");
        })
        .await;
    }

    #[tokio::test]
    async fn revoking_twice_is_refused_rather_than_silently_restamped() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let granted = grant(&db, &assistant(&h)).await.unwrap();
            revoke(&db, granted.id, h.firm).await.unwrap();

            let error = revoke(&db, granted.id, h.firm)
                .await
                .expect_err("a second revocation must not move the recorded date");
            assert!(matches!(error, DelegationError::AlreadyRevoked(id) if id == granted.id));
        })
        .await;
    }

    #[tokio::test]
    async fn a_revoked_grant_stays_revoked_whatever_the_roles_become() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let granted = grant(&db, &assistant(&h)).await.unwrap();
            let revoked = revoke(&db, granted.id, h.firm).await.unwrap();

            assert_eq!(
                revoked.state(Role::Client, Role::Client),
                DelegationState::Revoked,
                "revocation wins over dormancy",
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_pair_may_be_granted_again_after_a_revocation() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let first = grant(&db, &assistant(&h)).await.unwrap();
            revoke(&db, first.id, h.firm).await.unwrap();

            let second = grant(&db, &assistant(&h))
                .await
                .expect("a withdrawn delegation must not lock the pair out forever");

            assert_ne!(second.id, first.id, "re-granting writes a new row");
            assert_eq!(
                all_for_delegate(&db, h.delegate).await.unwrap().len(),
                2,
                "both the withdrawn row and the new one remain on the table",
            );
            assert_eq!(
                live_for_delegate(&db, h.delegate).await.unwrap(),
                vec![second],
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_second_live_grant_for_one_pair_is_refused() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            grant(&db, &assistant(&h)).await.unwrap();

            let error = grant(&db, &assistant(&h))
                .await
                .expect_err("one live delegation per pair");
            assert!(matches!(error, DelegationError::AlreadyGranted));
        })
        .await;
    }

    // --- The remaining write-path refusals.

    #[tokio::test]
    async fn self_delegation_is_refused() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;

            let error = grant(
                &db,
                &NewDelegation {
                    subject_person_id: h.delegate,
                    ..assistant(&h)
                },
            )
            .await
            .expect_err("a person cannot be their own delegate");
            assert!(matches!(error, DelegationError::SelfDelegation));
        })
        .await;
    }

    #[tokio::test]
    async fn a_basis_claiming_an_instrument_must_name_one() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;

            for basis in [
                Basis::PowerOfAttorney,
                Basis::Guardian,
                Basis::Executor,
                Basis::Trustee,
                Basis::ParentOfMinor,
            ] {
                assert!(basis.claims_an_instrument(), "{}", basis.as_str());

                let error = grant(
                    &db,
                    &NewDelegation {
                        basis,
                        instrument_reference: None,
                        ..assistant(&h)
                    },
                )
                .await
                .expect_err("a legal-instrument basis must cite its instrument");
                assert!(
                    matches!(error, DelegationError::InstrumentReferenceRequired(named)
                    if named == basis.as_str()),
                    "expected InstrumentReferenceRequired for {}, got {error:?}",
                    basis.as_str(),
                );

                // Whitespace is not a citation.
                let error = grant(
                    &db,
                    &NewDelegation {
                        basis,
                        instrument_reference: Some("   ".to_string()),
                        ..assistant(&h)
                    },
                )
                .await
                .expect_err("blank is not a citation");
                assert!(matches!(
                    error,
                    DelegationError::InstrumentReferenceRequired(_)
                ));
            }
        })
        .await;
    }

    #[tokio::test]
    async fn an_assistant_needs_no_instrument_because_it_asserts_none() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;

            assert!(!Basis::Assistant.claims_an_instrument());
            let granted = grant(&db, &assistant(&h)).await.unwrap();
            assert_eq!(granted.instrument_reference, None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_cited_instrument_round_trips() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;

            let granted = grant(
                &db,
                &NewDelegation {
                    basis: Basis::PowerOfAttorney,
                    instrument_reference: Some("Durable POA dated 2026-03-14".to_string()),
                    ..assistant(&h)
                },
            )
            .await
            .unwrap();

            assert_eq!(granted.basis, Basis::PowerOfAttorney);
            assert_eq!(
                granted.instrument_reference.as_deref(),
                Some("Durable POA dated 2026-03-14"),
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_missing_person_on_any_leg_is_refused() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let ghost = Uuid::now_v7();

            for input in [
                NewDelegation {
                    delegate_person_id: ghost,
                    ..assistant(&h)
                },
                NewDelegation {
                    subject_person_id: ghost,
                    ..assistant(&h)
                },
                NewDelegation {
                    granted_by_person_id: ghost,
                    ..assistant(&h)
                },
            ] {
                let error = grant(&db, &input)
                    .await
                    .expect_err("every person on the row must exist");
                assert!(
                    matches!(error, DelegationError::NoSuchPerson(id) if id == ghost),
                    "got {error:?}",
                );
            }
        })
        .await;
    }

    // --- Notice to the subject.

    #[tokio::test]
    async fn subject_notice_is_recorded_with_the_channel_it_travelled_on() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let granted = grant(&db, &assistant(&h)).await.unwrap();

            // The motivating case: the subject has no mailbox, so the firm
            // telephones them and records that it did.
            let notified = record_subject_notice(&db, granted.id, NoticeChannel::Telephone)
                .await
                .unwrap();

            assert!(notified.subject_was_notified());
            assert_eq!(
                notified.subject_notified_channel,
                Some(NoticeChannel::Telephone),
            );
            assert!(notified.subject_notified_at.is_some());
        })
        .await;
    }

    #[tokio::test]
    async fn notice_can_be_recorded_at_grant_time_too() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;

            let granted = grant(
                &db,
                &NewDelegation {
                    subject_notified_at: Some("2026-03-14T17:00:00Z".to_string()),
                    subject_notified_channel: Some(NoticeChannel::InPerson),
                    ..assistant(&h)
                },
            )
            .await
            .unwrap();

            assert!(granted.subject_was_notified());
            assert_eq!(
                granted.subject_notified_channel,
                Some(NoticeChannel::InPerson),
            );
        })
        .await;
    }

    #[tokio::test]
    async fn recording_notice_on_a_missing_delegation_is_refused() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let ghost = Uuid::now_v7();

            let error = record_subject_notice(&db, ghost, NoticeChannel::Telephone)
                .await
                .expect_err("no such delegation");
            assert!(matches!(error, DelegationError::NoSuchDelegation(id) if id == ghost));
        })
        .await;
    }

    // --- Reads.

    #[tokio::test]
    async fn the_subject_side_answers_who_can_act_as_me() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let other = client(&db, "Second Delegate", "second@example.com").await;
            let first = grant(&db, &assistant(&h)).await.unwrap();
            let second = grant(
                &db,
                &NewDelegation {
                    delegate_person_id: other,
                    ..assistant(&h)
                },
            )
            .await
            .unwrap();

            let mut live = live_for_subject(&db, h.subject).await.unwrap();
            live.sort_by_key(|row| row.id);
            let mut expected = vec![first, second];
            expected.sort_by_key(|row| row.id);
            assert_eq!(
                live, expected,
                "a client must be able to learn who may act as them without \
             holding a credential themselves",
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_person_with_no_delegations_reads_empty_rather_than_erroring() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let lonely = client(&db, "Unlinked", "unlinked@example.com").await;

            assert!(all_for_delegate(&db, lonely).await.unwrap().is_empty());
            assert!(all_for_subject(&db, lonely).await.unwrap().is_empty());
            assert!(live_for_delegate(&db, lonely).await.unwrap().is_empty());
            assert!(live_for_subject(&db, lonely).await.unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn reading_a_missing_person_is_an_error_not_an_empty_list() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let ghost = Uuid::now_v7();

            let error = live_for_delegate(&db, ghost).await.expect_err(
                "a delegate who does not exist is not the same as one with no \
             delegations",
            );
            assert!(matches!(error, DelegationError::NoSuchPerson(id) if id == ghost));
        })
        .await;
    }

    /// A referenced Person cannot be deleted while a delegation exists.
    #[tokio::test]
    async fn a_referenced_counterparty_cannot_be_deleted() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let recorded = grant(&db, &assistant(&h)).await.unwrap();
            assert!(persons::delete(&db, h.subject).await.is_err());
            assert_eq!(
                live_for_delegate(&db, h.delegate).await.unwrap(),
                vec![recorded]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn find_live_for_pair_sees_only_the_live_row() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            let first = grant(&db, &assistant(&h)).await.unwrap();

            assert_eq!(
                find_live_for_pair(&db, h.delegate, h.subject)
                    .await
                    .unwrap(),
                vec![first.clone()],
            );

            revoke(&db, first.id, h.firm).await.unwrap();
            assert!(find_live_for_pair(&db, h.delegate, h.subject)
                .await
                .unwrap()
                .is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_delegation_is_directional() {
        crate::test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let h = household(&db).await;
            grant(&db, &assistant(&h)).await.unwrap();

            assert!(
                find_live_for_pair(&db, h.subject, h.delegate)
                    .await
                    .unwrap()
                    .is_empty(),
                "helping someone does not entitle them to help you back",
            );
            assert!(
                live_for_delegate(&db, h.subject).await.unwrap().is_empty(),
                "the subject gains nothing as a delegate",
            );
        })
        .await;
    }

    // --- Enum round-trips, so a stored spelling and its type cannot drift.

    #[test]
    fn every_basis_round_trips_through_its_stored_spelling() {
        for basis in [
            Basis::Assistant,
            Basis::PowerOfAttorney,
            Basis::Guardian,
            Basis::Executor,
            Basis::Trustee,
            Basis::ParentOfMinor,
        ] {
            assert_eq!(Basis::parse(basis.as_str()), Some(basis));
        }
        assert_eq!(Basis::parse("attorney_in_fact"), None);
        assert_eq!(Basis::parse(""), None);
        assert_eq!(
            Basis::parse("  assistant  "),
            Some(Basis::Assistant),
            "stored spellings are trimmed, as `Role::parse` does",
        );
    }

    #[test]
    fn every_notice_channel_round_trips_through_its_stored_spelling() {
        for channel in [
            NoticeChannel::InPerson,
            NoticeChannel::Telephone,
            NoticeChannel::Post,
            NoticeChannel::Email,
        ] {
            assert_eq!(NoticeChannel::parse(channel.as_str()), Some(channel));
        }
        assert_eq!(NoticeChannel::parse("carrier_pigeon"), None);
    }
}
