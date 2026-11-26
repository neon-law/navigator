//! The id a third-party system issues for a Person — an address book,
//! and nothing more.
//!
//! # This table carries no authorization meaning
//!
//! A row here turns "this Person" into "this account" for an API call:
//! creating a repository and putting the right people on it means
//! telling GitHub *which* user, and the API wants an id, not an email
//! address. Inviting someone to a Slack channel, as a Docusign
//! envelope recipient, or into a Notion relation is the same problem.
//!
//! Three separate things are involved and this module owns exactly one:
//!
//! | | Lives where |
//! | -- | -- |
//! | **Who** — the account id to name in the call | here |
//! | **Credential** — what authenticates the call | firm-level service configuration, managed as secrets |
//! | **Authority** — whether the call should be made at all | `persons.role` and policy |
//!
//! So **no code may read this table to make an access decision.** That
//! is not a scoping convenience, it is the safety property.
//! [`crate::persons::Role`] is the authorization tier and
//! `person_project_roles.participation` is the scope; an external
//! identity is neither. A row saying a Clerk is GitHub user `12345`
//! must never be the reason that Clerk can read a repository — the
//! glossary rule that a Clerk "never receives lawyer-work, advice, Git,
//! MCP, or `/app/lawyer` authority by inheritance" holds precisely because
//! this table is inert, and `cli/tests/external_identity_is_inert.rs`
//! asserts that against the authorization surfaces by name rather than
//! leaving it to be remembered.
//!
//! Project participation likewise never grants source-forge access. The
//! rule is per-system, not per-role — a `client` Person holding a
//! `google` identity for Drive sharing is legitimate, and the same
//! Person is still never provisioned into the forge that hosts the
//! source — so the schema carries no blanket role constraint and the
//! enforcement belongs where provisioning happens. See
//! [`docs/access-model.md`](../../../docs/access-model.md) and
//! [`docs/project-repositories.md`](../../../docs/project-repositories.md).
//!
//! # The id, never the handle
//!
//! [`ExternalIdentity::external_id`] is the provider's **immutable** id:
//! a GitHub numeric id, a Slack `U…`, a Google `sub`, a Linear uuid, or a
//! Notion workspace user id.
//! [`ExternalIdentity::handle`] is display only and expected to drift.
//! Handles are renameable, and a mapping keyed on one breaks quietly —
//! the provisioning call simply fails to find a user, at exactly the
//! moment a matter is opening.
//!
//! # Values are unverified
//!
//! Navigator does not confirm that GitHub user `12345` really is this
//! Person. The identifier is entered or imported, the way
//! `person.xero_contact_id` already is. A wrong id is a data-entry bug
//! with data-entry consequences, and a stale row is wrong data rather
//! than a security incident — which is what makes reconciliation,
//! webhooks, and drift detection out of scope rather than missing.
//!
//! PostgreSQL owns identity writes. The explicit Source importer reads
//! captured identities directly, and Source read access remains available.

use chrono::{DateTime, Utc};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use sqlx::{postgres::PgRow, AssertSqlSafe, Row};

use crate::persons::{self, PersonError};
use crate::postgres::identity::{self as authority, AuthorityError};
use crate::surreal::{record_id, record_uuid, SurrealDb};

/// The table these rows live in.
const TABLE: &str = "person_external_identity";
/// The table `person_id` links into.
const PERSON_TABLE: &str = "person";

/// A third-party system Navigator can name a Person in.
///
/// Stored as a `string` with an `ASSERT $value IN [...]` on the field —
/// the same pair [`crate::persons::Role`] forms with `person.role`. The
/// vocabulary is closed: [`ExternalSystem::parse`] fails closed, with no
/// `Other(String)` escape, so an unknown system is refused rather than
/// resolved. Adding one costs a variant here and an entry in the ASSERT,
/// not a table.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ExternalSystem {
    /// GitHub / GitHub Enterprise. `external_id` is the numeric user id.
    ///
    /// One variant covers both, and naming GitHub Enterprise here is
    /// deliberate rather than left over: Navigator runs on github.com, and
    /// a self-hoster running it against their own tenant records ids issued
    /// by that tenant through this same variant. The system is the kind of
    /// forge, not one deployment's host.
    Github,
    /// Slack. `external_id` is the `U…` member id.
    Slack,
    /// Docusign. `external_id` is the envelope-recipient user id — not
    /// the firm-level `DOCUSIGN_USER_ID`, which is one impersonated API
    /// user and is service configuration rather than a per-Person map.
    Docusign,
    /// Google. `external_id` is the OIDC `sub`.
    Google,
    /// Linear. `external_id` is the user uuid.
    Linear,
    /// Claude. `external_id` is the workspace member id.
    Claude,
    /// ChatGPT. `external_id` is the workspace member id.
    Chatgpt,
    /// Notion. `external_id` is the stable workspace user id.
    Notion,
    /// Facebook. `external_id` is the Page-scoped sender id an inbound
    /// Meta webhook recorded — the same value `Conversation`'s
    /// `external_address` carries, so the inbox can say who is behind a
    /// bare id.
    Facebook,
    /// Instagram. `external_id` is the Instagram-scoped sender id an
    /// inbound Meta webhook recorded.
    Instagram,
    /// Threads. `external_id` is the Threads user id an inbound Meta
    /// webhook recorded.
    Threads,
    /// YouTube. `external_id` is the commenter's channel id, as the
    /// comments poller records it.
    Youtube,
    /// TikTok. `external_id` is the commenter id the Business comment
    /// webhook recorded.
    Tiktok,
}

impl ExternalSystem {
    /// Every system, in the order the schema's ASSERT lists them. The
    /// pairing is held by
    /// [`the_assert_and_the_enum_name_the_same_systems`].
    pub const ALL: &'static [Self] = &[
        Self::Github,
        Self::Slack,
        Self::Docusign,
        Self::Google,
        Self::Linear,
        Self::Claude,
        Self::Chatgpt,
        Self::Notion,
        Self::Facebook,
        Self::Instagram,
        Self::Threads,
        Self::Youtube,
        Self::Tiktok,
    ];

    /// The stored spelling — what goes in the column and what the ASSERT
    /// gates.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Slack => "slack",
            Self::Docusign => "docusign",
            Self::Google => "google",
            Self::Linear => "linear",
            Self::Claude => "claude",
            Self::Chatgpt => "chatgpt",
            Self::Notion => "notion",
            Self::Facebook => "facebook",
            Self::Instagram => "instagram",
            Self::Threads => "threads",
            Self::Youtube => "youtube",
            Self::Tiktok => "tiktok",
        }
    }

    /// Read a stored spelling back, or `None` when it names no system
    /// this build knows.
    ///
    /// Fail-closed on purpose, and deliberately without an
    /// `Other(String)` variant to fall into. A system Navigator cannot
    /// name is a system Navigator cannot call, so resolving it to
    /// *something* would only move the failure to the call site with a
    /// value that looks legitimate.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|system| system.as_str() == value)
    }
}

impl std::fmt::Display for ExternalSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors reading or writing an external identity.
#[derive(Debug, thiserror::Error)]
pub enum ExternalIdentityError {
    /// A database operation failed.
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// Reading the person a write links to failed.
    #[error(transparent)]
    Person(#[from] PersonError),
    /// The Person named by a write does not exist.
    #[error("no person {0}")]
    NoSuchPerson(Uuid),
    /// Another person already holds this account — the write collided
    /// with `person_external_identity_account`.
    #[error("that {system} account is already held by another person")]
    AccountTaken { system: ExternalSystem },
    /// This person already holds an account on this system — the write
    /// collided with `person_external_identity_person_system`.
    #[error("that person already holds a {system} identity")]
    SystemAlreadyLinked { system: ExternalSystem },
    /// A write reported success but returned no row, or returned one
    /// this module could not read back — see
    /// [`ExternalIdentityRow::into_identity`].
    #[error("writing an external identity returned no usable row")]
    WriteReturnedNothing,
    /// PostgreSQL identity authority is configured and failed.
    #[error(transparent)]
    Authority(#[from] AuthorityError),
}

fn from_pg(row: &PgRow) -> Result<ExternalIdentity, sqlx::Error> {
    let system: String = row.try_get("system")?;
    Ok(ExternalIdentity {
        id: row.try_get("id")?,
        person_id: row.try_get("person_id")?,
        system: ExternalSystem::parse(&system).ok_or_else(|| sqlx::Error::ColumnDecode {
            index: "system".into(),
            source: Box::new(std::io::Error::other("unknown external system")),
        })?,
        external_id: row.try_get("external_id")?,
        handle: row.try_get("handle")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn pg_error(error: sqlx::Error, system: ExternalSystem) -> ExternalIdentityError {
    if authority::violates(&error, "person_external_identity_system_external_id_key") {
        ExternalIdentityError::AccountTaken { system }
    } else if authority::violates(&error, "person_external_identity_person_id_system_key") {
        ExternalIdentityError::SystemAlreadyLinked { system }
    } else {
        ExternalIdentityError::Authority(error.into())
    }
}

/// One identity row from PostgreSQL matching `predicate` over `$1` and `$2`.
async fn pg_one(
    pg: &crate::postgres::PostgresDb,
    predicate: &str,
    first: String,
    second: String,
    system: ExternalSystem,
) -> Result<Option<ExternalIdentity>, ExternalIdentityError> {
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE {predicate} LIMIT 1"
    )))
    .bind(first)
    .bind(second)
    .fetch_optional(&pg.0)
    .await
    .map_err(|error| pg_error(error, system))?;
    row.as_ref()
        .map(from_pg)
        .transpose()
        .map_err(|error| pg_error(error, system))
}

/// External identity writes require admitted PostgreSQL identity authority.
async fn write_authority() -> Result<crate::postgres::PostgresDb, ExternalIdentityError> {
    authority::authority().await?.ok_or_else(|| {
        AuthorityError::Unavailable(
            "external identity writes are unavailable in Source context".into(),
        )
        .into()
    })
}

/// One Person's account on one external system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalIdentity {
    pub id: Uuid,
    pub person_id: Uuid,
    pub system: ExternalSystem,
    /// The provider's immutable id — the value an API call names.
    pub external_id: String,
    /// The display handle, if one was recorded. Expected to drift; never
    /// the key.
    pub handle: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as the engine reads and writes it. Separate from
/// [`ExternalIdentity`] because the SDK owns its own `RecordId` and
/// `Datetime`, and `system` arrives as the stored string; the conversion
/// belongs at this seam rather than in every caller.
#[derive(SurrealValue)]
struct ExternalIdentityRow {
    id: surrealdb::types::RecordId,
    person_id: surrealdb::types::RecordId,
    system: String,
    external_id: String,
    handle: Option<String>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl ExternalIdentityRow {
    /// `None` when a record id is not a native UUID key (see
    /// [`crate::surreal`] for why the two key spellings differ) or when
    /// `system` is not one the vocabulary names. Both are rows this
    /// workspace could not have written; reporting them would invent an
    /// id or a system.
    fn into_identity(self) -> Option<ExternalIdentity> {
        Some(ExternalIdentity {
            id: record_uuid(&self.id)?,
            person_id: record_uuid(&self.person_id)?,
            system: ExternalSystem::parse(&self.system)?,
            external_id: self.external_id,
            handle: self.handle,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`ExternalIdentityRow`] from only one
/// query.
const SELECT: &str = "id, person_id, system, external_id, handle, inserted_at, updated_at";

/// Record that `person_id` is `external_id` on `system`.
///
/// `handle` is stored as given and never matched on.
///
/// # Errors
///
/// [`ExternalIdentityError::NoSuchPerson`] when the person does not
/// exist — checked before the database foreign key.
/// [`ExternalIdentityError::AccountTaken`] when another person already
/// holds this account, and
/// [`ExternalIdentityError::SystemAlreadyLinked`] when this person
/// already holds one on this system. [`ExternalIdentityError::Authority`]
/// reports PostgreSQL failures.
pub async fn link(
    db: &SurrealDb,
    person_id: Uuid,
    system: ExternalSystem,
    external_id: &str,
    handle: Option<&str>,
) -> Result<ExternalIdentity, ExternalIdentityError> {
    let pg = write_authority().await?;
    if persons::find_by_id(db, person_id).await?.is_none() {
        return Err(ExternalIdentityError::NoSuchPerson(person_id));
    }

    let id = Uuid::now_v7();
    let row = sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {TABLE} (id, person_id, system, external_id, handle) \
             VALUES ($1, $2, $3, $4, $5) RETURNING {SELECT}"
    )))
    .bind(id)
    .bind(person_id)
    .bind(system.as_str())
    .bind(external_id.trim())
    .bind(handle)
    .fetch_one(&pg.0)
    .await
    .map_err(|error| pg_error(error, system))?;
    let identity = from_pg(&row).map_err(|error| pg_error(error, system))?;

    Ok(identity)
}

/// This person's account on this system, if one is recorded. The pair is
/// the natural key `person_external_identity_person_system` carries.
///
/// # Errors
///
/// [`ExternalIdentityError::Db`] if the lookup fails.
pub async fn find_by_person_and_system(
    db: &SurrealDb,
    person_id: Uuid,
    system: ExternalSystem,
) -> Result<Option<ExternalIdentity>, ExternalIdentityError> {
    if let Some(pg) = authority::authority().await? {
        return Box::pin(pg_one(
            &pg,
            "person_id = $1::uuid AND system = $2",
            person_id.to_string(),
            system.as_str().to_string(),
            system,
        ))
        .await;
    }
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} \
             WHERE person_id = $person_id AND system = $system \
             LIMIT 1"
        ))
        .bind(("person_id", record_id(PERSON_TABLE, person_id)))
        .bind(("system", system.as_str().to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;

    let row: Option<ExternalIdentityRow> = response.take(0)?;
    Ok(row.and_then(ExternalIdentityRow::into_identity))
}

/// The reverse lookup: who is `system` account `external_id`? An indexed
/// hit on `person_external_identity_account`.
///
/// Finding a row does **not** mean the person may be added to anything —
/// see the module header. This resolves an id; authority is a separate
/// question answered elsewhere.
///
/// # Errors
///
/// [`ExternalIdentityError::Db`] if the lookup fails.
pub async fn find_by_account(
    db: &SurrealDb,
    system: ExternalSystem,
    external_id: &str,
) -> Result<Option<ExternalIdentity>, ExternalIdentityError> {
    if let Some(pg) = authority::authority().await? {
        return Box::pin(pg_one(
            &pg,
            "system = $1 AND external_id = $2",
            system.as_str().to_string(),
            external_id.trim().to_string(),
            system,
        ))
        .await;
    }
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} \
             WHERE system = $system AND external_id = $external_id \
             LIMIT 1"
        ))
        .bind(("system", system.as_str().to_string()))
        .bind(("external_id", external_id.trim().to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;

    let row: Option<ExternalIdentityRow> = response.take(0)?;
    Ok(row.and_then(ExternalIdentityRow::into_identity))
}

/// Resolve the Person named by an external account id.
///
/// This is the provisioning-side relation resolver. It deliberately accepts
/// only the provider's stable account id and never takes, searches, or falls
/// back to a display name or email. Finding a Person here does not authorize
/// any action; the caller still applies the provider and Navigator policy
/// before making an outbound call.
///
/// # Errors
///
/// [`ExternalIdentityError::Db`] if either lookup fails.
pub async fn find_person_by_account(
    db: &SurrealDb,
    system: ExternalSystem,
    external_id: &str,
) -> Result<Option<persons::Person>, ExternalIdentityError> {
    let Some(identity) = find_by_account(db, system, external_id).await? else {
        return Ok(None);
    };
    persons::find_by_id(db, identity.person_id)
        .await
        .map_err(ExternalIdentityError::Person)
}

/// The Notion user ids for every attorney DRI on a Project.
///
/// The Projects database's `Lead` relation is a set: every lawyer DRI is
/// included, in stable external-id order. The relation is resolved from the
/// Notion identity rows only; names and email addresses are never used as
/// relation keys. A DRI without a Notion identity is omitted so an
/// unconfigured account cannot be mistaken for another user.
///
/// # Errors
///
/// [`ExternalIdentityError::Db`] if the participation or identity lookup
/// fails.
pub async fn notion_user_ids_for_project_leads(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<String>, ExternalIdentityError> {
    let participations = crate::projects::participations_for_project(db, project_id)
        .await
        .map_err(|_| {
            ExternalIdentityError::Authority(AuthorityError::Unavailable(
                "Project participation unavailable".into(),
            ))
        })?;
    let mut notion_ids = Vec::new();
    for participation in participations.into_iter().filter(|row| row.is_lawyer_dri) {
        let person_id = participation.person_id;
        if let Some(identity) =
            find_by_person_and_system(db, person_id, ExternalSystem::Notion).await?
        {
            notion_ids.push(identity.external_id);
        }
    }
    notion_ids.sort();
    notion_ids.dedup();
    Ok(notion_ids)
}

/// Every identity recorded for one person, oldest first.
///
/// # Errors
///
/// [`ExternalIdentityError::Db`] if the lookup fails.
pub async fn for_person(
    db: &SurrealDb,
    person_id: Uuid,
) -> Result<Vec<ExternalIdentity>, ExternalIdentityError> {
    if let Some(pg) = authority::authority().await? {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE person_id = $1 ORDER BY inserted_at, id"
        )))
        .bind(person_id)
        .fetch_all(&pg.0)
        .await
        .map_err(AuthorityError::from)?;
        return Ok(rows
            .iter()
            .map(from_pg)
            .collect::<Result<_, _>>()
            .map_err(AuthorityError::from)?);
    }
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE person_id = $person_id ORDER BY inserted_at ASC"
        ))
        .bind(("person_id", record_id(PERSON_TABLE, person_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;

    let rows: Vec<ExternalIdentityRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(ExternalIdentityRow::into_identity)
        .collect())
}

/// Link the identity unless this person already holds one on this
/// system. Race-safe without a lock, the same way
/// [`crate::credentials::find_or_grant`] is: a concurrent writer that
/// wins `person_external_identity_person_system` turns this call's
/// insert into [`ExternalIdentityError::SystemAlreadyLinked`], which is
/// re-read as the winner's row.
///
/// An existing row is returned as it stands — a differing `external_id`
/// does not overwrite it. Repointing a Person at a different account is
/// a correction someone should make deliberately through [`unlink`] and
/// [`link`], not a silent effect of the next import.
///
/// # Errors
///
/// [`ExternalIdentityError::NoSuchPerson`] as [`link`] documents,
/// [`ExternalIdentityError::AccountTaken`] when the account belongs to
/// somebody else — a real conflict, not a race — and
/// [`ExternalIdentityError::Db`] if a lookup or the insert fails.
pub async fn find_or_link(
    db: &SurrealDb,
    person_id: Uuid,
    system: ExternalSystem,
    external_id: &str,
    handle: Option<&str>,
) -> Result<ExternalIdentity, ExternalIdentityError> {
    if let Some(existing) = find_by_person_and_system(db, person_id, system).await? {
        return Ok(existing);
    }
    match link(db, person_id, system, external_id, handle).await {
        Ok(linked) => Ok(linked),
        Err(ExternalIdentityError::SystemAlreadyLinked { .. }) => {
            find_by_person_and_system(db, person_id, system)
                .await?
                .ok_or(ExternalIdentityError::WriteReturnedNothing)
        }
        Err(error) => Err(error),
    }
}

/// Set or clear one Person's identity on one external system.
///
/// Setting an existing identity updates its immutable account id only after
/// the database has checked the `(system, external_id)` unique index. Clearing
/// removes the row. This is the deliberate correction path for an admin or an
/// import; [`find_or_link`] intentionally keeps its existing-idempotent,
/// never-repoint behavior.
///
/// # Errors
///
/// [`ExternalIdentityError::NoSuchPerson`] when the Person does not exist,
/// [`ExternalIdentityError::AccountTaken`] when another Person holds the
/// account, or [`ExternalIdentityError::Authority`] for a PostgreSQL failure.
pub async fn set_for_person(
    db: &SurrealDb,
    person_id: Uuid,
    system: ExternalSystem,
    external_id: Option<&str>,
) -> Result<Option<ExternalIdentity>, ExternalIdentityError> {
    let pg = write_authority().await?;
    if persons::find_by_id(db, person_id).await?.is_none() {
        return Err(ExternalIdentityError::NoSuchPerson(person_id));
    }
    let external_id = external_id.map(str::trim).filter(|id| !id.is_empty());
    let existing = find_by_person_and_system(db, person_id, system).await?;

    let Some(external_id) = external_id else {
        if let Some(identity) = existing {
            unlink(db, identity.id).await?;
        }
        return Ok(None);
    };

    if let Some(identity) = existing {
        if identity.external_id == external_id {
            return Ok(Some(identity));
        }
        let row = sqlx::query(AssertSqlSafe(format!(
            "UPDATE {TABLE} SET external_id = $2, updated_at = now() \
                 WHERE id = $1 RETURNING {SELECT}"
        )))
        .bind(identity.id)
        .bind(external_id)
        .fetch_optional(&pg.0)
        .await
        .map_err(|error| pg_error(error, system))?
        .ok_or(ExternalIdentityError::WriteReturnedNothing)?;
        let updated = from_pg(&row).map_err(|error| pg_error(error, system))?;

        return Ok(Some(updated));
    }

    link(db, person_id, system, external_id, None)
        .await
        .map(Some)
}

/// Forget an identity — the account was closed, or recorded in error.
/// Idempotent: unlinking one that is not there is a no-op.
///
/// # Errors
///
/// [`ExternalIdentityError::Authority`] if the delete fails.
pub async fn unlink(_db: &SurrealDb, identity_id: Uuid) -> Result<(), ExternalIdentityError> {
    let pg = write_authority().await?;
    sqlx::query(AssertSqlSafe(format!("DELETE FROM {TABLE} WHERE id = $1")))
        .bind(identity_id)
        .execute(&pg.0)
        .await
        .map_err(AuthorityError::from)?;
    Ok(())
}

#[cfg(test)]
#[path = "external_identities/native_tests.rs"]
mod native_tests;

#[cfg(test)]
#[path = "external_identities/compatibility_tests.rs"]
mod compatibility_tests;
