//! Owning-practice records and firm membership.
//!
//! A [`Firm`] is the ownership boundary for Projects and for the people who
//! work them. It sits beneath the house-brand registry: a brand is which
//! storefront a client walked through; a firm is which practice owns the
//! matter. A Firm is an Entity: [`Firm::entity_id`] is the legal person that
//! practice is. [`firm_brand`](attach_brand) records which closed house-brand
//! keys that practice wears.
//!
//! Membership is a join table, shaped like `person_project_role`. It does
//! not replace `person.role`. The deployment-wide Owner tier stays on
//! `person`; a client reaches a matter through `person_project_role` and
//! does not get a firm-membership row.
//!
//! A Firm's [`slug`] is derived from its name once, at creation, and never
//! changes; it names the Firm's Forgejo organization ([`forgejo`]).

mod calendar;
pub(crate) use calendar::calendar_owners_query;
pub use calendar::{calendar_owner, with_google_calendar};

pub mod forgejo;
pub mod forgejo_organization;
pub mod slug;

use serde::Serialize;
use std::collections::HashMap;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::persons::Role;
use crate::postgres_mirror::MirrorKey;
use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

pub(crate) const TABLE: &str = "firm";
const MEMBERSHIP_TABLE: &str = "person_firm_role";
const PERSON_TABLE: &str = "person";
const BRAND_TABLE: &str = "firm_brand";
const ENTITY_TABLE: &str = "entity";
const FIRM_SELECT: &str =
    "id, name, slug, status, entity_id, portal_theme_brand_key, inserted_at, updated_at";
const MEMBERSHIP_SELECT: &str =
    "id, person_id, firm_id, membership, is_dri, inserted_at, updated_at";

/// The compiled house-brand keys `store::seed` migrates into `brand` rows
/// on first boot (ENG-496). Matches the `ASSERT` on `firm_brand.brand_key`
/// and `project.brand`. `store` does not depend on `views`, so this is the
/// string form of `BrandKey::ALL`. [`attach_brand`] no longer validates
/// against this list directly: it reads the `brand` table
/// ([`brand_key_exists`]), which this constant seeds, so a Firm may wear
/// any key a `brand` row now names, runtime-created ones included.
pub const CLOSED_BRAND_KEYS: &[&str] = &[
    "neon",
    "delete-your-data",
    "delete-your-debt",
    "vesta",
    "misericordia",
    "abhaya",
    "summons",
    "daybridge",
    "cyber-injury-law",
    "aegis",
    "groundwork",
    "guieb-shook",
    "iep-defenders",
    "lex-tecnica",
    "same-day-legal",
];

/// Whether a `brand` row exists carrying this key — the live check
/// [`attach_brand`] replaced its closed-list `ASSERT` with (ENG-496).
/// `pub(crate)` so `store::projects::create` can run the identical check
/// (ENG-587) rather than a second, divergent closed list.
pub(crate) async fn brand_key_exists(
    surreal: &SurrealDb,
    brand_key: &str,
) -> Result<bool, FirmError> {
    #[derive(SurrealValue)]
    struct BrandKeyRow {
        brand_key: String,
    }
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::brand_key_exists(brand_key)).await?);
    }
    let mut response = surreal
        .query("SELECT brand_key FROM brand WHERE brand_key = $brand_key LIMIT 1")
        .bind(("brand_key", brand_key.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<BrandKeyRow> = response.take(0)?;
    Ok(!rows.is_empty())
}

/// A practice that owns Projects and firm-side people.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Firm {
    pub id: Uuid,
    pub name: String,
    /// The Firm's immutable slug, derived from [`Self::name`] at creation
    /// ([`slug::derive`]). Renaming the Firm leaves it unchanged.
    pub slug: String,
    pub status: String,
    /// The legal Entity this practice is. `None` only on a historical row
    /// written before `entity_id` existed.
    pub entity_id: Option<Uuid>,
    /// The [Portal Theme Brand](../../docs/glossary/portal-theme-brand.md):
    /// the key of the Brand whose tokens, logo, and name identify the Firm
    /// to its team. Clients use the Brand of the host where they sign in.
    /// `None` keeps the host's own Brand for everyone.
    pub portal_theme_brand_key: Option<String>,
    pub inserted_at: String,
    pub updated_at: String,
}

#[derive(SurrealValue)]
struct FirmRow {
    id: surrealdb::types::RecordId,
    name: String,
    slug: String,
    status: String,
    entity_id: Option<surrealdb::types::RecordId>,
    portal_theme_brand_key: Option<String>,
    inserted_at: String,
    updated_at: String,
}

impl FirmRow {
    fn into_firm(self) -> Option<Firm> {
        Some(Firm {
            id: record_uuid(&self.id)?,
            name: self.name,
            slug: self.slug,
            status: self.status,
            entity_id: match self.entity_id.as_ref() {
                Some(id) => Some(record_uuid(id)?),
                None => None,
            },
            portal_theme_brand_key: self.portal_theme_brand_key,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(SurrealValue)]
struct ProjectIdRow {
    id: surrealdb::types::RecordId,
}

#[derive(SurrealValue)]
struct PersonIdRow {
    person_id: surrealdb::types::RecordId,
}

#[derive(SurrealValue)]
struct TouchedProject {
    id: surrealdb::types::RecordId,
    brand: String,
}

/// Inputs for creating a [`Firm`].
///
/// Creation is atomic with an initial Admin DRI (ENG-499): there is no
/// setup state a Firm passes through before it has one.
/// `admin_dri_person_id` must name a person carrying `person.role = admin`;
/// [`create`] refuses anything else rather than leaving the new Firm without
/// its one accountable administrator.
#[derive(Debug, Clone)]
pub struct NewFirm {
    pub name: String,
    pub status: String,
    pub entity_id: Uuid,
    /// The person who becomes this Firm's first Admin DRI, in the same
    /// transaction that creates the Firm.
    pub admin_dri_person_id: Uuid,
}

/// Which membership a person holds at a firm.
///
/// Distinct from [`crate::persons::Role`]: that enum is the system-wide
/// authorization tier, including Owner and Client. This closed set is
/// only the practice-side memberships a `person_firm_role` row may
/// carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum FirmMembership {
    Admin,
    Lawyer,
    Clerk,
}

impl FirmMembership {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Lawyer => "lawyer",
            Self::Clerk => "clerk",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "admin" => Some(Self::Admin),
            "lawyer" => Some(Self::Lawyer),
            "clerk" => Some(Self::Clerk),
            _ => None,
        }
    }

    /// The practice membership that corresponds to a system-wide [`Role`].
    /// Owner and Client have none: Owner is deployment-wide, and a client
    /// reaches a matter through `person_project_role`.
    #[must_use]
    pub fn for_role(role: Role) -> Option<Self> {
        match role {
            Role::Admin => Some(Self::Admin),
            Role::Lawyer => Some(Self::Lawyer),
            Role::Clerk => Some(Self::Clerk),
            Role::Owner | Role::Client => None,
        }
    }
}

/// One person's membership at a firm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonFirmRole {
    pub id: Uuid,
    pub person_id: Uuid,
    pub firm_id: Uuid,
    pub membership: FirmMembership,
    pub is_dri: bool,
    pub inserted_at: String,
    pub updated_at: String,
}

#[derive(SurrealValue)]
struct PersonFirmRoleRow {
    id: surrealdb::types::RecordId,
    person_id: surrealdb::types::RecordId,
    firm_id: surrealdb::types::RecordId,
    membership: String,
    is_dri: bool,
    inserted_at: String,
    updated_at: String,
}

impl PersonFirmRoleRow {
    fn into_role(self) -> Option<PersonFirmRole> {
        Some(PersonFirmRole {
            id: record_uuid(&self.id)?,
            person_id: record_uuid(&self.person_id)?,
            firm_id: record_uuid(&self.firm_id)?,
            membership: FirmMembership::parse(&self.membership)?,
            is_dri: self.is_dri,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

/// Inputs for creating a [`PersonFirmRole`].
#[derive(Debug, Clone)]
pub struct NewPersonFirmRole {
    pub person_id: Uuid,
    pub firm_id: Uuid,
    pub membership: FirmMembership,
    pub is_dri: bool,
}

/// Errors from the firm command seam.
#[derive(Debug, thiserror::Error)]
pub enum FirmError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error(transparent)]
    Postgres(#[from] crate::postgres::tenancy::TenancyError),
    #[error("PostgreSQL firm operation failed")]
    PostgresSql(#[from] sqlx::Error),
    #[error(transparent)]
    Person(#[from] crate::persons::PersonError),
    #[error(transparent)]
    Entity(#[from] crate::entities::EntityError),
    #[error("writing a firm returned no usable row")]
    WriteReturnedNothing,
    #[error("Firm Calendar settings storage is unavailable")]
    CalendarSettingsUnavailable,
    #[error("Enter a valid Google Workspace mailbox address.")]
    InvalidCalendarOwner,
    #[error("Gmail addresses are not supported. Choose a Google Workspace mailbox on the Firm's own domain.")]
    GmailCalendarOwner,
    #[error("Choose DocuSign or Dropbox Sign.")]
    InvalidEsignProvider,
    #[error("no person {0}")]
    NoSuchPerson(Uuid),
    #[error("person {0} is a client and cannot hold a firm membership")]
    ClientCannotJoinFirm(Uuid),
    #[error("no firm {0}")]
    NoSuchFirm(Uuid),
    #[error("no entity {0}")]
    NoSuchEntity(Uuid),
    #[error("that person is already a member of this firm")]
    DuplicateMembership,
    #[error("that entity already has a firm")]
    DuplicateEntity,
    /// The Firm's name derives no valid slug: nothing slug-shaped is left
    /// once it is folded to ASCII, or the result is reserved
    /// ([`slug::is_valid`]).
    #[error("firm name derives the invalid slug {0:?}")]
    InvalidSlug(String),
    /// Another Firm already holds the slug this name derives.
    #[error("another firm already holds the slug {0:?}")]
    DuplicateSlug(String),
    #[error("unknown brand key {0}")]
    UnknownBrand(String),
    #[error("that brand is already attached to a firm")]
    DuplicateBrand,
    /// The proposed Admin DRI carries neither `person.role = admin` nor
    /// `person.role = owner`. Refused for Lawyer, Clerk, and Client alike
    /// (ENG-499).
    #[error("person {0} does not hold the admin or owner role and cannot be an Admin DRI")]
    IneligibleAdminDriTier(Uuid),
    /// The person holds a `person_firm_role` row on this Firm, but its
    /// membership is not `admin`.
    #[error("person {0} is not an admin member of firm {1}")]
    WrongAdminDriMembership(Uuid, Uuid),
    /// The person holds no membership row on this Firm at all. Distinct from
    /// [`Self::WrongAdminDriMembership`]: this person is not a member of the
    /// target Firm, whatever membership they hold elsewhere.
    #[error("person {0} is not a member of firm {1}")]
    AdminDriCrossFirm(Uuid, Uuid),
    /// Only Owner may appoint or transfer a Firm's Admin DRI.
    #[error("only Owner may appoint or transfer a Firm's Admin DRI")]
    NotAuthorized,
    /// Removing this membership, or changing it away from `admin`, would
    /// leave an active Firm with no Admin DRI. Transfer the designation
    /// first with [`appoint_admin_dri`].
    #[error("firm {0} would be left without an Admin DRI")]
    WouldLeaveFirmWithoutAdminDri(Uuid),
    /// This Firm still owns one or more Projects; deletion is refused
    /// rather than orphaning a matter's owning practice (ENG-494).
    #[error("firm {0} still owns one or more projects and cannot be deleted")]
    FirmOwnsProjects(Uuid),
    #[error("firm {0} still owns one or more brands and cannot be deleted")]
    FirmOwnsBrands(Uuid),
    /// The person holds no `person_firm_role` row on this Firm — there is
    /// nothing for [`update_membership`] or [`remove_membership`] to change.
    #[error("person {0} is not a member of firm {1}")]
    NotAMember(Uuid, Uuid),
    /// The Brand row has no owning Firm, so there is no Firm it may be
    /// attached to through [`attach_own_brand`].
    #[error("brand {0} belongs to no firm")]
    BrandHasNoFirm(String),
    /// Reading the `brand` row failed. Boxed because
    /// [`crate::brands::BrandError`] itself carries a `FirmError`.
    #[error(transparent)]
    Brand(Box<crate::brands::BrandError>),
    /// The proposed Portal Theme Brand is not a Brand this Firm owns.
    #[error("firm {0} does not own brand {1}")]
    ThemeBrandNotOwned(Uuid, String),
}

impl FirmError {
    /// The message to show a human on the Firm create/edit forms. Kept here
    /// so every adapter — this form today, any future JSON API or MCP tool —
    /// renders the same wording per refusal, the same convention
    /// `EntityCommandError::user_message` and `PeopleCommandError::user_message`
    /// already follow.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::NoSuchEntity(_) => {
                "Pick an existing entity to open the firm against.".to_string()
            }
            Self::NoSuchPerson(_) => "That person could not be found.".to_string(),
            Self::IneligibleAdminDriTier(_) => {
                "The Admin DRI must be a person whose role is admin or owner.".to_string()
            }
            Self::DuplicateEntity => "That entity already has a firm.".to_string(),
            Self::InvalidSlug(_) => {
                "Name the firm with letters or digits, and not a reserved word such as `admin`."
                    .to_string()
            }
            Self::DuplicateSlug(slug) => format!(
                "Another firm already uses the organization name `{slug}`. Choose a different name."
            ),
            Self::NotAuthorized => "You may not make this change.".to_string(),
            Self::NoSuchFirm(_) => "That firm could not be found.".to_string(),
            Self::ClientCannotJoinFirm(_) => "A client cannot hold a firm membership.".to_string(),
            Self::DuplicateMembership => {
                "That person is already a member of this firm.".to_string()
            }
            Self::UnknownBrand(_) => "That brand key is not registered.".to_string(),
            Self::DuplicateBrand => "That brand is already attached to a firm.".to_string(),
            Self::WrongAdminDriMembership(_, _) => {
                "That person's membership on this firm is not admin.".to_string()
            }
            Self::AdminDriCrossFirm(_, _) => {
                "That person is not a member of this firm.".to_string()
            }
            Self::WouldLeaveFirmWithoutAdminDri(_) => {
                "This firm must always have an Admin DRI. Transfer the designation first."
                    .to_string()
            }
            Self::FirmOwnsProjects(_) => {
                "This firm still owns one or more projects and cannot be deleted.".to_string()
            }
            Self::FirmOwnsBrands(_) => {
                "This firm still owns one or more brands and cannot be deleted.".to_string()
            }
            Self::NotAMember(_, _) => "That person is not a member of this firm.".to_string(),
            Self::BrandHasNoFirm(_) => "That brand belongs to no firm.".to_string(),
            Self::ThemeBrandNotOwned(_, _) => {
                "The portal theme must be one of this firm's own brands.".to_string()
            }
            Self::CalendarSettingsUnavailable
            | Self::InvalidCalendarOwner
            | Self::GmailCalendarOwner
            | Self::InvalidEsignProvider => self.to_string(),
            Self::Db(_)
            | Self::Postgres(_)
            | Self::PostgresSql(_)
            | Self::Person(_)
            | Self::Entity(_)
            | Self::Brand(_)
            | Self::WriteReturnedNothing => "Could not save firm.".to_string(),
        }
    }
}

fn classify_write(error: surrealdb::Error) -> FirmError {
    match crate::surreal::retry::unique_violation(&error) {
        Some("person_firm_role_pair") => FirmError::DuplicateMembership,
        Some("firm_entity") => FirmError::DuplicateEntity,
        Some("firm_brand_key" | "firm_brand_pair") => FirmError::DuplicateBrand,
        _ => FirmError::Db(error),
    }
}

async fn writing<F, Q>(attempt: F) -> Result<surrealdb::IndexedResults, FirmError>
where
    F: FnMut() -> Q,
    Q: std::future::IntoFuture<Output = Result<surrealdb::IndexedResults, surrealdb::Error>>,
{
    retry::writing(attempt).await.map_err(classify_write)
}

/// Create a firm under a fresh UUID record key, atomically with its first
/// Admin DRI.
///
/// `admin_dri_person_id` must resolve to a person carrying
/// `person.role = admin` or `person.role = owner`; anything else — Lawyer,
/// Clerk, Client, or a dangling id — is refused before either row is written, so a Firm is never
/// created without one (ENG-499: there is no setup state). The Firm row and
/// its `person_firm_role` membership (carrying `is_dri = true`) are written
/// in one transaction, so a reader never observes a Firm with zero Admin
/// DRIs.
///
/// The Firm's [`Firm::slug`] is derived from its name here, once. A name that
/// derives an invalid or reserved slug is refused as
/// [`FirmError::InvalidSlug`], and one whose slug another Firm already holds
/// as [`FirmError::DuplicateSlug`]; no suffix is added to make it fit.
pub async fn create(surreal: &SurrealDb, input: &NewFirm) -> Result<Firm, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Box::pin(crate::postgres::tenancy::create_firm(input)).await;
    }
    if crate::entities::find_by_id(surreal, input.entity_id)
        .await?
        .is_none()
    {
        return Err(FirmError::NoSuchEntity(input.entity_id));
    }
    let admin = crate::persons::find_by_id(surreal, input.admin_dri_person_id)
        .await?
        .ok_or(FirmError::NoSuchPerson(input.admin_dri_person_id))?;
    if !admin.role.is_admin_tier() {
        return Err(FirmError::IneligibleAdminDriTier(input.admin_dri_person_id));
    }
    let slug = slug::derive(&input.name);
    if !slug::is_valid(&slug) {
        return Err(FirmError::InvalidSlug(slug));
    }
    let firm_id = Uuid::now_v7();
    let membership_id = Uuid::now_v7();
    let now = chrono::Utc::now().to_rfc3339();
    let mut response = retry::writing(|| {
        surreal
            .query(format!(
                "BEGIN; \
                 CREATE $firm_id SET name = $name, slug = $slug, status = $status, \
                 entity_id = $entity_id, inserted_at = $now, updated_at = $now \
                 RETURN {FIRM_SELECT}; \
                 CREATE $membership_id SET person_id = $person_id, firm_id = $firm_id, \
                 membership = 'admin', is_dri = true, inserted_at = $now, updated_at = $now; \
                 COMMIT;"
            ))
            .bind(("firm_id", record_id(TABLE, firm_id)))
            .bind(("membership_id", record_id(MEMBERSHIP_TABLE, membership_id)))
            .bind((
                "person_id",
                record_id(PERSON_TABLE, input.admin_dri_person_id),
            ))
            .bind(("name", input.name.clone()))
            .bind(("slug", slug.clone()))
            .bind(("status", input.status.clone()))
            .bind(("entity_id", record_id(ENTITY_TABLE, input.entity_id)))
            .bind(("now", now.clone()))
    })
    .await
    .map_err(|error| {
        if retry::unique_violation(&error) == Some("firm_slug") {
            FirmError::DuplicateSlug(slug.clone())
        } else {
            classify_write(error)
        }
    })?;
    let row: Option<FirmRow> = response.take(1)?;
    let firm = row
        .and_then(FirmRow::into_firm)
        .ok_or(FirmError::WriteReturnedNothing)?;
    crate::postgres_mirror::after_writes(
        surreal,
        [
            MirrorKey::Firm(firm.id),
            MirrorKey::FirmMembership {
                person_id: input.admin_dri_person_id,
                firm_id: firm.id,
            },
        ],
    )
    .await;
    Ok(firm)
}

/// Find the firm identified by `id`.
pub async fn find_by_id(surreal: &SurrealDb, id: Uuid) -> Result<Option<Firm>, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::find_firm(id)).await?);
    }
    let mut response = surreal
        .query(format!("SELECT {FIRM_SELECT} FROM ONLY $id"))
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<FirmRow> = response.take(0)?;
    let firm = row.and_then(FirmRow::into_firm);
    Ok(firm)
}

/// Find the firm whose [`Firm::entity_id`] is `entity_id`.
pub async fn find_by_entity_id(
    surreal: &SurrealDb,
    entity_id: Uuid,
) -> Result<Option<Firm>, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::firm_by_entity(entity_id)).await?);
    }
    let mut response = surreal
        .query(format!(
            "SELECT {FIRM_SELECT} FROM {TABLE} WHERE entity_id = $entity_id LIMIT 1"
        ))
        .bind(("entity_id", record_id(ENTITY_TABLE, entity_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<FirmRow> = response.take(0)?;
    Ok(rows.into_iter().find_map(FirmRow::into_firm))
}

/// Every firm, name then id.
pub async fn all(surreal: &SurrealDb) -> Result<Vec<Firm>, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::all_firms()).await?);
    }
    let mut response = surreal
        .query(format!(
            "SELECT {FIRM_SELECT} FROM {TABLE} ORDER BY name, id"
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<FirmRow> = response.take(0)?;
    Ok(rows.into_iter().filter_map(FirmRow::into_firm).collect())
}

/// A partial edit to a [`Firm`]'s own fields (ENG-494). Absent fields are
/// left untouched; this never touches `person_firm_role` or `firm_brand`.
#[derive(Debug, Clone, Default)]
pub struct FirmEdit {
    pub name: Option<String>,
    /// `active`, `suspended`, or `archived`.
    pub status: Option<String>,
    pub entity_id: Option<Uuid>,
    /// `Some(Some(key))` sets the [`Firm::portal_theme_brand_key`] to a Brand
    /// this Firm owns (`brand.firm_id`); `Some(None)` clears it.
    pub portal_theme_brand_key: Option<Option<String>>,
}

/// Gate a Firm-scoped settings/membership/brand write through
/// [`crate::firm_capability::FirmCapability::ManageMembership`] — the same
/// capability that already gates who may add a member, since editing a
/// Firm's own settings, its people's memberships, and the brands it wears
/// are one administrative surface (ENG-494). Owner passes on every Firm;
/// only the Admin membership tier passes on its own.
async fn authorize_manage_firm(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
) -> Result<(), FirmError> {
    use crate::firm_capability::{resolve, FirmCapability, FirmCapabilityDecision};
    match resolve(
        surreal,
        actor_role,
        actor_person_id,
        firm_id,
        FirmCapability::ManageMembership,
    )
    .await?
    {
        FirmCapabilityDecision::Allowed => Ok(()),
        FirmCapabilityDecision::FirmNotFound => Err(FirmError::NoSuchFirm(firm_id)),
        FirmCapabilityDecision::Forbidden => Err(FirmError::NotAuthorized),
    }
}

/// Whether `address` is a consumer Gmail address. Calendar runs on Google
/// Workspace delegation, which a consumer account cannot grant.
#[must_use]
pub fn is_consumer_gmail(address: &str) -> bool {
    address.rsplit_once('@').is_some_and(|(_, domain)| {
        let domain = domain.trim();
        domain.eq_ignore_ascii_case("gmail.com") || domain.eq_ignore_ascii_case("googlemail.com")
    })
}

/// Save the Owner's Google Workspace mailbox choice. A blank choice disables Calendar
/// for the Firm and suppresses the deployment bootstrap.
pub async fn set_calendar_owner(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    value: &str,
) -> Result<(), FirmError> {
    if actor_role != Role::Owner {
        return Err(FirmError::NotAuthorized);
    }
    authorize_manage_firm(surreal, actor_role, actor_person_id, firm_id).await?;
    let value = value.trim();
    let owner = if value.is_empty() {
        None
    } else {
        let valid = value.split_once('@').is_some_and(|(mailbox, domain)| {
            !mailbox.is_empty()
                && !domain.contains('@')
                && domain.split('.').count() > 1
                && domain.split('.').all(|part| {
                    !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
                && mailbox
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".+_-".contains(&b))
        });
        if !valid || value.len() > 254 {
            return Err(FirmError::InvalidCalendarOwner);
        }
        if is_consumer_gmail(value) {
            return Err(FirmError::GmailCalendarOwner);
        }
        Some(value)
    };
    let db = crate::postgres::PostgresDb::application()
        .await
        .map_err(|_| FirmError::CalendarSettingsUnavailable)?;
    sqlx::query(
        "INSERT INTO firm_calendar_settings (firm_id, google_workspace_owner) VALUES ($1, $2)
         ON CONFLICT (firm_id) DO UPDATE SET google_workspace_owner = EXCLUDED.google_workspace_owner",
    )
    .bind(firm_id)
    .bind(owner)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// Gate an Owner-only brand-seat write through the capability resolver —
/// the audit point every Firm-scoped allow/deny decision routes through.
/// `MoveBrand` admits no membership tier: moving a Brand to another Firm
/// re-points whose legal entity the public site's copyright names, an
/// ownership decision only Owner makes, where `authorize_manage_firm`
/// stays the gate for a Firm's own brand administration.
async fn authorize_brand_ownership(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    capability: crate::firm_capability::FirmCapability,
) -> Result<(), FirmError> {
    use crate::firm_capability::{resolve, FirmCapabilityDecision};
    match resolve(surreal, actor_role, actor_person_id, firm_id, capability).await? {
        FirmCapabilityDecision::Allowed => Ok(()),
        FirmCapabilityDecision::FirmNotFound => Err(FirmError::NoSuchFirm(firm_id)),
        FirmCapabilityDecision::Forbidden => Err(FirmError::NotAuthorized),
    }
}

/// Edit a Firm's own fields. Owner, or that Firm's own Admin membership,
/// only (`authorize_manage_firm`). Refuses an `entity_id` that does not
/// resolve, the same guarantee [`create`] gives; leaves the Admin DRI and
/// every membership row untouched — those move only through
/// [`appoint_admin_dri`], [`update_membership`], and [`remove_membership`].
pub async fn update(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    input: &FirmEdit,
) -> Result<Firm, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Box::pin(crate::postgres::tenancy::update_firm(
            actor_role,
            actor_person_id,
            firm_id,
            input,
        ))
        .await;
    }
    authorize_manage_firm(surreal, actor_role, actor_person_id, firm_id).await?;
    if let Some(entity_id) = input.entity_id {
        if crate::entities::find_by_id(surreal, entity_id)
            .await?
            .is_none()
        {
            return Err(FirmError::NoSuchEntity(entity_id));
        }
    }
    if let Some(Some(key)) = &input.portal_theme_brand_key {
        let owner = crate::brands::find_by_key(surreal, key)
            .await
            .map_err(|error| FirmError::Brand(Box::new(error)))?
            .and_then(|brand| brand.firm_id);
        if owner != Some(firm_id) {
            return Err(FirmError::ThemeBrandNotOwned(firm_id, key.clone()));
        }
    }

    let mut assignments: Vec<&str> = vec!["updated_at = $updated_at"];
    if input.name.is_some() {
        assignments.push("name = $name");
    }
    if input.status.is_some() {
        assignments.push("status = $status");
    }
    if input.entity_id.is_some() {
        assignments.push("entity_id = $entity_id");
    }
    if input.portal_theme_brand_key.is_some() {
        assignments.push("portal_theme_brand_key = $portal_theme_brand_key");
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut response = writing(|| {
        surreal
            .query(format!(
                "UPDATE $id SET {} RETURN {FIRM_SELECT}",
                assignments.join(", ")
            ))
            .bind(("id", record_id(TABLE, firm_id)))
            .bind(("updated_at", now.clone()))
            .bind(("name", input.name.clone()))
            .bind(("status", input.status.clone()))
            .bind((
                "entity_id",
                input.entity_id.map(|id| record_id(ENTITY_TABLE, id)),
            ))
            .bind((
                "portal_theme_brand_key",
                input.portal_theme_brand_key.clone().flatten(),
            ))
    })
    .await?;
    let row: Option<FirmRow> = response.take(0)?;
    let firm = row
        .and_then(FirmRow::into_firm)
        .ok_or(FirmError::WriteReturnedNothing)?;
    crate::postgres_mirror::after_write(surreal, MirrorKey::Firm(firm.id)).await;
    Ok(firm)
}

/// Delete a Firm. Owner, or that Firm's own Admin membership, only
/// (`authorize_manage_firm`). Refused when any Project still names it as
/// `firm_id`: a matter must never lose its owning practice — or while any
/// Brand row names it as owner, which would orphan that Brand. Its
/// `person_firm_role` and `firm_brand` rows are removed in the same
/// transaction — nothing is left pointing at a Firm that no longer exists.
pub async fn delete(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
) -> Result<(), FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Box::pin(crate::postgres::tenancy::delete_firm(
            actor_role,
            actor_person_id,
            firm_id,
        ))
        .await;
    }
    authorize_manage_firm(surreal, actor_role, actor_person_id, firm_id).await?;
    let mut projects = surreal
        .query("SELECT id FROM project WHERE firm_id = $firm_id LIMIT 1")
        .bind(("firm_id", record_id(TABLE, firm_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let owned: Vec<ProjectIdRow> = projects.take(0)?;
    if !owned.is_empty() {
        return Err(FirmError::FirmOwnsProjects(firm_id));
    }
    if !crate::brands::for_firm(surreal, firm_id)
        .await
        .map_err(|error| FirmError::Brand(Box::new(error)))?
        .is_empty()
    {
        return Err(FirmError::FirmOwnsBrands(firm_id));
    }
    let mut removed: Vec<MirrorKey> = memberships_for_firm(surreal, firm_id)
        .await?
        .into_iter()
        .map(|role| MirrorKey::FirmMembership {
            person_id: role.person_id,
            firm_id,
        })
        .collect();
    removed.extend(
        brand_keys_for_firm(surreal, firm_id)
            .await?
            .into_iter()
            .map(|brand_key| MirrorKey::FirmBrand { brand_key }),
    );
    removed.push(MirrorKey::Firm(firm_id));

    writing(|| {
        surreal
            .query(
                "BEGIN; \
                 DELETE person_firm_role WHERE firm_id = $firm_id; \
                 DELETE firm_brand WHERE firm_id = $firm_id; \
                 DELETE $firm_id; \
                 COMMIT;",
            )
            .bind(("firm_id", record_id(TABLE, firm_id)))
    })
    .await?;
    crate::postgres_mirror::after_writes(surreal, removed).await;
    Ok(())
}

/// Record one person's membership at a firm.
///
/// Reads both referenced rows before writing: a `record<>` link constrains
/// the target table but does not prove the row exists. A client-tier person
/// reaches a matter through `person_project_role`, never through firm
/// membership, so this refuses to write a row for one regardless of the
/// requested `membership` value.
pub async fn add_membership(
    surreal: &SurrealDb,
    input: &NewPersonFirmRole,
) -> Result<PersonFirmRole, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Box::pin(crate::postgres::tenancy::add_membership(input)).await;
    }
    let person = crate::persons::find_by_id(surreal, input.person_id)
        .await?
        .ok_or(FirmError::NoSuchPerson(input.person_id))?;
    if person.role == Role::Client {
        return Err(FirmError::ClientCannotJoinFirm(input.person_id));
    }
    if find_by_id(surreal, input.firm_id).await?.is_none() {
        return Err(FirmError::NoSuchFirm(input.firm_id));
    }
    let now = chrono::Utc::now().to_rfc3339();
    let mut response = writing(|| {
        surreal
            .query(format!(
                "CREATE $id SET person_id = $person_id, firm_id = $firm_id, \
                 membership = $membership, is_dri = $is_dri, \
                 inserted_at = $now, updated_at = $now RETURN {MEMBERSHIP_SELECT}"
            ))
            .bind(("id", record_id(MEMBERSHIP_TABLE, Uuid::now_v7())))
            .bind(("person_id", record_id(PERSON_TABLE, input.person_id)))
            .bind(("firm_id", record_id(TABLE, input.firm_id)))
            .bind(("membership", input.membership.as_str().to_string()))
            .bind(("is_dri", input.is_dri))
            .bind(("now", now.clone()))
    })
    .await?;
    let row: Option<PersonFirmRoleRow> = response.take(0)?;
    let role = row
        .and_then(PersonFirmRoleRow::into_role)
        .ok_or(FirmError::WriteReturnedNothing)?;
    crate::postgres_mirror::after_write(
        surreal,
        MirrorKey::FirmMembership {
            person_id: role.person_id,
            firm_id: role.firm_id,
        },
    )
    .await;
    Ok(role)
}

/// The membership row for this `(person, firm)` pair, if any.
pub async fn membership_for_person(
    surreal: &SurrealDb,
    person_id: Uuid,
    firm_id: Uuid,
) -> Result<Option<PersonFirmRole>, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::membership(person_id, firm_id)).await?);
    }
    let mut response = surreal
        .query(format!(
            "SELECT {MEMBERSHIP_SELECT} FROM ONLY {MEMBERSHIP_TABLE} \
             WHERE person_id = $person_id AND firm_id = $firm_id LIMIT 1"
        ))
        .bind(("person_id", record_id(PERSON_TABLE, person_id)))
        .bind(("firm_id", record_id(TABLE, firm_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<PersonFirmRoleRow> = response.take(0)?;
    Ok(row.and_then(PersonFirmRoleRow::into_role))
}

/// Every membership row for this person.
pub async fn memberships_for_person(
    surreal: &SurrealDb,
    person_id: Uuid,
) -> Result<Vec<PersonFirmRole>, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::memberships_for_person(person_id)).await?);
    }
    let mut response = surreal
        .query(format!(
            "SELECT {MEMBERSHIP_SELECT} FROM {MEMBERSHIP_TABLE} \
             WHERE person_id = $person_id ORDER BY inserted_at, id"
        ))
        .bind(("person_id", record_id(PERSON_TABLE, person_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<PersonFirmRoleRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(PersonFirmRoleRow::into_role)
        .collect())
}

/// Every membership row on this Firm — the inverse of
/// [`memberships_for_person`], for a Firm detail view listing who belongs to
/// it rather than which Firms one person belongs to.
pub async fn memberships_for_firm(
    surreal: &SurrealDb,
    firm_id: Uuid,
) -> Result<Vec<PersonFirmRole>, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::memberships_for_firm(firm_id)).await?);
    }
    let mut response = surreal
        .query(format!(
            "SELECT {MEMBERSHIP_SELECT} FROM {MEMBERSHIP_TABLE} \
             WHERE firm_id = $firm_id ORDER BY inserted_at, id"
        ))
        .bind(("firm_id", record_id(TABLE, firm_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<PersonFirmRoleRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(PersonFirmRoleRow::into_role)
        .collect())
}

/// Change a person's `membership` at a Firm. Owner, or that Firm's own Admin
/// membership, only (`authorize_manage_firm`). Leaves `is_dri` untouched — a
/// membership form must never write that marker; only [`appoint_admin_dri`]
/// does. Refused with [`FirmError::WouldLeaveFirmWithoutAdminDri`] when the
/// person is the Firm's current sole Admin DRI and `membership` is not
/// `admin` (ENG-499's guard, [`refuse_admin_dri_orphaning`]).
pub async fn update_membership(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    person_id: Uuid,
    firm_id: Uuid,
    membership: FirmMembership,
) -> Result<PersonFirmRole, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Box::pin(crate::postgres::tenancy::update_membership(
            actor_role,
            actor_person_id,
            person_id,
            firm_id,
            membership,
        ))
        .await;
    }
    authorize_manage_firm(surreal, actor_role, actor_person_id, firm_id).await?;
    if membership_for_person(surreal, person_id, firm_id)
        .await?
        .is_none()
    {
        return Err(FirmError::NotAMember(person_id, firm_id));
    }
    refuse_admin_dri_orphaning(surreal, firm_id, person_id, Some(membership)).await?;

    let now = chrono::Utc::now().to_rfc3339();
    writing(|| {
        surreal
            .query(
                "UPDATE person_firm_role SET membership = $membership, updated_at = $now \
                 WHERE person_id = $person_id AND firm_id = $firm_id",
            )
            .bind(("person_id", record_id(PERSON_TABLE, person_id)))
            .bind(("firm_id", record_id(TABLE, firm_id)))
            .bind(("membership", membership.as_str().to_string()))
            .bind(("now", now.clone()))
    })
    .await?;
    crate::postgres_mirror::after_write(surreal, MirrorKey::FirmMembership { person_id, firm_id })
        .await;

    membership_for_person(surreal, person_id, firm_id)
        .await?
        .ok_or(FirmError::WriteReturnedNothing)
}

/// Remove a person's membership at a Firm entirely. Owner, or that Firm's
/// own Admin membership, only (`authorize_manage_firm`). Refused with
/// [`FirmError::WouldLeaveFirmWithoutAdminDri`] when the person is the
/// Firm's current sole Admin DRI (ENG-499's guard,
/// [`refuse_admin_dri_orphaning`]) — transfer the designation first with
/// [`appoint_admin_dri`].
pub async fn remove_membership(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    person_id: Uuid,
    firm_id: Uuid,
) -> Result<(), FirmError> {
    remove_membership_row(surreal, actor_role, actor_person_id, person_id, firm_id).await?;
    // A Person who no longer belongs to a Firm wants no SSH keys on the
    // forge (ENG-1219). Best-effort: the operator sweep retries a failure.
    Box::pin(crate::ssh_public_keys::after_membership_removed(
        surreal, person_id,
    ))
    .await;
    Ok(())
}

async fn remove_membership_row(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    person_id: Uuid,
    firm_id: Uuid,
) -> Result<(), FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Box::pin(crate::postgres::tenancy::remove_membership(
            actor_role,
            actor_person_id,
            person_id,
            firm_id,
        ))
        .await;
    }
    authorize_manage_firm(surreal, actor_role, actor_person_id, firm_id).await?;
    if membership_for_person(surreal, person_id, firm_id)
        .await?
        .is_none()
    {
        return Err(FirmError::NotAMember(person_id, firm_id));
    }
    refuse_admin_dri_orphaning(surreal, firm_id, person_id, None).await?;

    writing(|| {
        surreal
            .query("DELETE person_firm_role WHERE person_id = $person_id AND firm_id = $firm_id")
            .bind(("person_id", record_id(PERSON_TABLE, person_id)))
            .bind(("firm_id", record_id(TABLE, firm_id)))
    })
    .await?;
    crate::postgres_mirror::after_write(surreal, MirrorKey::FirmMembership { person_id, firm_id })
        .await;
    Ok(())
}

/// Person ids an Admin of these firms may see: members of those firms, plus
/// anyone with a `person_project_role` on a matter those firms own.
///
/// Routes through [`crate::firm_capability::allowed_firm_ids`] with
/// [`crate::firm_capability::FirmCapability::ViewDirectory`] (ENG-463) rather
/// than deriving its own membership set, so this stays in step with every
/// other Firm-scoped directory read.
pub async fn visible_person_ids(
    surreal: &SurrealDb,
    admin_person_id: Uuid,
) -> Result<Vec<Uuid>, FirmError> {
    let firm_ids = crate::firm_capability::allowed_firm_ids(
        surreal,
        Role::Admin,
        Some(admin_person_id),
        crate::firm_capability::FirmCapability::ViewDirectory,
    )
    .await?;
    if firm_ids.is_empty() {
        return Ok(Vec::new());
    }
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::visible_person_ids(&firm_ids)).await?);
    }
    let firm_records: Vec<_> = firm_ids
        .iter()
        .copied()
        .map(|id| record_id(TABLE, id))
        .collect();
    let mut members = surreal
        .query(format!(
            "SELECT {MEMBERSHIP_SELECT} FROM {MEMBERSHIP_TABLE} \
             WHERE firm_id IN $firm_ids"
        ))
        .bind(("firm_ids", firm_records.clone()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let member_rows: Vec<PersonFirmRoleRow> = members.take(0)?;
    let mut ids: std::collections::BTreeSet<Uuid> = member_rows
        .into_iter()
        .filter_map(PersonFirmRoleRow::into_role)
        .map(|row| row.person_id)
        .collect();

    let mut projects = surreal
        .query("SELECT id FROM project WHERE firm_id IN $firm_ids")
        .bind(("firm_ids", firm_records))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let project_rows: Vec<ProjectIdRow> = projects.take(0)?;
    let project_ids: Vec<_> = project_rows.into_iter().map(|row| row.id).collect();
    if !project_ids.is_empty() {
        let mut participants = surreal
            .query("SELECT person_id FROM person_project_role WHERE project_id IN $project_ids")
            .bind(("project_ids", project_ids))
            .await
            .and_then(surrealdb::IndexedResults::check)?;
        let participant_rows: Vec<PersonIdRow> = participants.take(0)?;
        for row in participant_rows {
            if let Some(id) = record_uuid(&row.person_id) {
                ids.insert(id);
            }
        }
    }
    Ok(ids.into_iter().collect())
}

/// Whose rows the people directory lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryScope {
    /// Every Person — the Owner's view.
    Everyone,
    /// Only the people [`visible_person_ids`] admits for this Admin.
    VisibleToAdmin(Uuid),
}

/// One people-directory row: the Person and the names of the Firms they hold
/// a membership on, in membership order.
#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub person: crate::persons::Person,
    pub firm_names: Vec<String>,
}

/// What the people directory accepts: `filter[name]`, `filter[email]`, and
/// `sort` on `name` / `email`, name order by default.
pub const PEOPLE_DIRECTORY: crate::listing::ListSpec = crate::listing::ListSpec {
    filters: &["name", "email"],
    sorts: &["name", "email"],
    default_sort: &[("name", false)],
};

/// One keyset page of the people directory (`/app/admin/people`,
/// `GET /app/api/people`) with each row's Firm names. Filters, sort, and the
/// page bound all run in the query; the Firm names are read for the page's
/// rows.
///
/// Under PostgreSQL People authority this is one query over the
/// `person_directory` and `firm_visible_person` views. Otherwise SurrealDB
/// reads the page and this joins the memberships and Firms read once each —
/// never a membership lookup per Person.
///
/// # Errors
///
/// [`FirmError`] when any of the reads fails.
pub async fn people_directory(
    surreal: &SurrealDb,
    params: &crate::listing::ListParams,
    scope: DirectoryScope,
) -> Result<crate::listing::Page<DirectoryEntry>, FirmError> {
    if let Some(pg) = crate::persons::postgres_authority().await? {
        let firm_ids = match scope {
            DirectoryScope::Everyone => None,
            DirectoryScope::VisibleToAdmin(admin) => Some(
                crate::firm_capability::allowed_firm_ids(
                    surreal,
                    Role::Admin,
                    Some(admin),
                    crate::firm_capability::FirmCapability::ViewDirectory,
                )
                .await?,
            ),
        };
        let page = Box::pin(crate::postgres::people::directory_page(
            &pg,
            params,
            firm_ids.as_deref(),
        ))
        .await?;
        return Ok(page.map(|(person, firm_names)| DirectoryEntry { person, firm_names }));
    }

    let visible = match scope {
        DirectoryScope::Everyone => None,
        DirectoryScope::VisibleToAdmin(admin) => Some(visible_person_ids(surreal, admin).await?),
    };
    let page = crate::persons::surreal_directory_page(surreal, params, visible.as_deref()).await?;
    let on_page: std::collections::HashSet<Uuid> =
        page.items.iter().map(|person| person.id).collect();
    let firm_names: HashMap<Uuid, String> = all(surreal)
        .await?
        .into_iter()
        .map(|firm| (firm.id, firm.name))
        .collect();
    let mut memberships = all_memberships(surreal).await?;
    memberships.retain(|membership| on_page.contains(&membership.person_id));
    memberships.sort_by(|a, b| (&a.inserted_at, a.id).cmp(&(&b.inserted_at, b.id)));
    let mut names_by_person: HashMap<Uuid, Vec<String>> = HashMap::new();
    for membership in memberships {
        if let Some(name) = firm_names.get(&membership.firm_id) {
            names_by_person
                .entry(membership.person_id)
                .or_default()
                .push(name.clone());
        }
    }
    Ok(page.map(|person| DirectoryEntry {
        firm_names: names_by_person.remove(&person.id).unwrap_or_default(),
        person,
    }))
}

/// Attach one of the deployment's closed house-brand keys to a Firm.
/// Owner only (`authorize_brand_ownership`): pointing the brand seat at a
/// firm is the same ownership decision moving it is — an Admin-DRI may
/// detach ([`detach_brand`]) but never attach, and an attach that carries
/// no authority is the same privilege hole a move would be. Crate-internal
/// flows that already hold the brand seat — the seed bootstrap and
/// [`ensure_brand`] — write through [`attach_brand_row`].
pub async fn attach_brand(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    brand_key: &str,
) -> Result<(), FirmError> {
    if crate::postgres::tenancy::enabled() {
        if actor_role != Role::Owner {
            return Err(FirmError::NotAuthorized);
        }
        return Box::pin(crate::postgres::tenancy::attach_brand(firm_id, brand_key)).await;
    }
    authorize_brand_ownership(
        surreal,
        actor_role,
        actor_person_id,
        firm_id,
        crate::firm_capability::FirmCapability::AttachBrand,
    )
    .await?;
    attach_brand_row(surreal, firm_id, brand_key).await
}

/// The raw brand-seat write `attach_brand` gates: validates the brand key
/// and the firm, then creates the `firm_brand` row. Crate-internal — the
/// seed bootstrap and flows like [`ensure_brand`] that already hold the
/// brand seat attach through this without re-walking Owner authority.
pub(crate) async fn attach_brand_row(
    surreal: &SurrealDb,
    firm_id: Uuid,
    brand_key: &str,
) -> Result<(), FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Box::pin(crate::postgres::tenancy::attach_brand(firm_id, brand_key)).await;
    }
    if !brand_key_exists(surreal, brand_key).await? {
        return Err(FirmError::UnknownBrand(brand_key.to_string()));
    }
    if find_by_id(surreal, firm_id).await?.is_none() {
        return Err(FirmError::NoSuchFirm(firm_id));
    }
    let now = chrono::Utc::now().to_rfc3339();
    writing(|| {
        surreal
            .query(
                "CREATE $id SET firm_id = $firm_id, brand_key = $brand_key, \
                 inserted_at = $now, updated_at = $now",
            )
            .bind(("id", record_id(BRAND_TABLE, Uuid::now_v7())))
            .bind(("firm_id", record_id(TABLE, firm_id)))
            .bind(("brand_key", brand_key.to_string()))
            .bind(("now", now.clone()))
    })
    .await?;
    crate::postgres_mirror::after_write(
        surreal,
        MirrorKey::FirmBrand {
            brand_key: brand_key.to_string(),
        },
    )
    .await;
    Ok(())
}

/// Attach `brand_key` if it is not already on this firm. A key already
/// worn by this firm is a no-op; a key worn by another firm is still an
/// error.
pub async fn ensure_brand(
    surreal: &SurrealDb,
    firm_id: Uuid,
    brand_key: &str,
) -> Result<(), FirmError> {
    let existing = brand_keys_for_firm(surreal, firm_id).await?;
    if existing.iter().any(|key| key == brand_key) {
        return Ok(());
    }
    match attach_brand_row(surreal, firm_id, brand_key).await {
        Ok(()) => Ok(()),
        Err(FirmError::DuplicateBrand) => {
            let keys = brand_keys_for_firm(surreal, firm_id).await?;
            if keys.iter().any(|key| key == brand_key) {
                Ok(())
            } else {
                Err(FirmError::DuplicateBrand)
            }
        }
        Err(error) => Err(error),
    }
}

/// House-brand keys this firm wears, in registry order.
pub async fn brand_keys_for_firm(
    surreal: &SurrealDb,
    firm_id: Uuid,
) -> Result<Vec<String>, FirmError> {
    #[derive(SurrealValue)]
    struct BrandRow {
        brand_key: String,
    }
    if crate::postgres::tenancy::enabled() {
        let keys = Box::pin(crate::postgres::tenancy::firm_brand_keys(firm_id)).await?;
        return Ok(in_registry_order(keys));
    }
    let mut response = surreal
        .query(format!(
            "SELECT brand_key FROM {BRAND_TABLE} WHERE firm_id = $firm_id ORDER BY brand_key"
        ))
        .bind(("firm_id", record_id(TABLE, firm_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<BrandRow> = response.take(0)?;
    Ok(in_registry_order(
        rows.into_iter().map(|row| row.brand_key).collect(),
    ))
}

/// The Firm that wears `brand_key`, if any. A key is unique across the
/// deployment (`firm_brand_key`), so this is at most one row.
pub async fn firm_id_for_brand_key(
    surreal: &SurrealDb,
    brand_key: &str,
) -> Result<Option<Uuid>, FirmError> {
    #[derive(SurrealValue)]
    struct FirmIdRow {
        firm_id: surrealdb::types::RecordId,
    }
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::firm_for_brand_key(brand_key)).await?);
    }
    let mut response = surreal
        .query(format!(
            "SELECT firm_id FROM {BRAND_TABLE} WHERE brand_key = $brand_key LIMIT 1"
        ))
        .bind(("brand_key", brand_key.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<FirmIdRow> = response.take(0)?;
    Ok(rows.into_iter().find_map(|row| record_uuid(&row.firm_id)))
}

/// One `firm_brand` row: a Brand key a Firm wears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FirmBrand {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub brand_key: String,
    pub inserted_at: String,
    pub updated_at: String,
}

#[derive(SurrealValue)]
struct FirmBrandRow {
    id: surrealdb::types::RecordId,
    firm_id: surrealdb::types::RecordId,
    brand_key: String,
    inserted_at: String,
    updated_at: String,
}

impl FirmBrandRow {
    fn into_firm_brand(self) -> Option<FirmBrand> {
        Some(FirmBrand {
            id: record_uuid(&self.id)?,
            firm_id: record_uuid(&self.firm_id)?,
            brand_key: self.brand_key,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

const FIRM_BRAND_SELECT: &str = "id, firm_id, brand_key, inserted_at, updated_at";

/// The `firm_brand` row for `brand_key`, if a Firm wears it. A key is unique
/// across the deployment, so this is at most one row.
pub async fn firm_brand_for_key(
    surreal: &SurrealDb,
    brand_key: &str,
) -> Result<Option<FirmBrand>, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::firm_brand_row_for_key(brand_key)).await?);
    }
    let mut response = surreal
        .query(format!(
            "SELECT {FIRM_BRAND_SELECT} FROM {BRAND_TABLE} WHERE brand_key = $brand_key LIMIT 1"
        ))
        .bind(("brand_key", brand_key.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<FirmBrandRow> = response.take(0)?;
    Ok(rows.into_iter().find_map(FirmBrandRow::into_firm_brand))
}

/// Every `firm_brand` row, by Brand key.
pub async fn all_firm_brands(surreal: &SurrealDb) -> Result<Vec<FirmBrand>, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::all_firm_brand_rows()).await?);
    }
    let mut response = surreal
        .query(format!(
            "SELECT {FIRM_BRAND_SELECT} FROM {BRAND_TABLE} ORDER BY brand_key"
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<FirmBrandRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(FirmBrandRow::into_firm_brand)
        .collect())
}

/// Every membership row on every Firm.
pub async fn all_memberships(surreal: &SurrealDb) -> Result<Vec<PersonFirmRole>, FirmError> {
    if crate::postgres::tenancy::enabled() {
        return Ok(Box::pin(crate::postgres::tenancy::all_memberships()).await?);
    }
    let mut response = surreal
        .query(format!(
            "SELECT {MEMBERSHIP_SELECT} FROM {MEMBERSHIP_TABLE} ORDER BY firm_id, person_id"
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<PersonFirmRoleRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(PersonFirmRoleRow::into_role)
        .collect())
}

/// House-brand keys this firm wears, ordered as [`CLOSED_BRAND_KEYS`] then
/// any runtime keys the Firm added later.
fn in_registry_order(keys: Vec<String>) -> Vec<String> {
    let mut ordered = Vec::new();
    for known in CLOSED_BRAND_KEYS {
        if keys.iter().any(|key| key == *known) {
            ordered.push((*known).to_string());
        }
    }
    for key in keys {
        if !ordered.contains(&key) {
            ordered.push(key);
        }
    }
    ordered
}

/// Attach a Brand to the Firm that owns its `brand` row. Owner, or that
/// Firm's own Admin membership, only (`authorize_manage_firm`). The Firm is
/// read from the row, never from the caller, so no one can make a Firm wear
/// another Firm's Brand. A key the Firm already wears is a no-op
/// ([`ensure_brand`]); returns the Firm the Brand is now attached to.
pub async fn attach_own_brand(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    brand_key: &str,
) -> Result<Uuid, FirmError> {
    let brand = crate::brands::find_by_key(surreal, brand_key)
        .await
        .map_err(|error| FirmError::Brand(Box::new(error)))?
        .ok_or_else(|| FirmError::UnknownBrand(brand_key.to_string()))?;
    let firm_id = brand
        .firm_id
        .ok_or_else(|| FirmError::BrandHasNoFirm(brand_key.to_string()))?;
    if crate::postgres::tenancy::enabled() {
        let pg = crate::postgres::PostgresDb::application()
            .await
            .map_err(crate::postgres::tenancy::TenancyError::from)?;
        Box::pin(crate::postgres::tenancy::authorize_firm_management(
            pg.pool(),
            actor_role,
            actor_person_id,
            firm_id,
        ))
        .await?;
    } else {
        authorize_manage_firm(surreal, actor_role, actor_person_id, firm_id).await?;
    }
    ensure_brand(surreal, firm_id, brand_key).await?;
    Ok(firm_id)
}

/// Detach a house-brand key from a firm. Owner, or that Firm's own Admin
/// membership, only (`authorize_manage_firm`). A key the firm does not wear
/// is a no-op — there is nothing to remove and nothing to refuse.
pub async fn detach_brand(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    brand_key: &str,
) -> Result<(), FirmError> {
    if crate::postgres::tenancy::enabled() {
        let pg = crate::postgres::PostgresDb::application()
            .await
            .map_err(crate::postgres::tenancy::TenancyError::from)?;
        Box::pin(crate::postgres::tenancy::authorize_firm_management(
            pg.pool(),
            actor_role,
            actor_person_id,
            firm_id,
        ))
        .await?;
        return Box::pin(crate::postgres::tenancy::detach_brand(firm_id, brand_key)).await;
    }
    authorize_manage_firm(surreal, actor_role, actor_person_id, firm_id).await?;
    writing(|| {
        surreal
            .query(format!(
                "DELETE {BRAND_TABLE} WHERE firm_id = $firm_id AND brand_key = $brand_key"
            ))
            .bind(("firm_id", record_id(TABLE, firm_id)))
            .bind(("brand_key", brand_key.to_string()))
    })
    .await?;
    crate::postgres_mirror::after_write(
        surreal,
        MirrorKey::FirmBrand {
            brand_key: brand_key.to_string(),
        },
    )
    .await;
    Ok(())
}

/// Move one Brand's seat to another Firm. Owner only, through the
/// capability resolver — `MoveBrand` admits no membership tier, because
/// moving a Brand changes whose legal entity the public site's copyright
/// names: an ownership decision only Owner makes, where an Admin-DRI may
/// still detach ([`detach_brand`]). One transaction removes the old
/// `firm_brand` row, re-points the `brand` row's owning Firm, and creates
/// the target Firm's row together, so a partial failure leaves the old
/// association intact and the UNIQUE index on `firm_brand.brand_key`
/// never trips on a half-state. The decision anchors to the target Firm —
/// a move completes only when the target exists — and unknown brand keys
/// are typed errors before any write runs. A move to the Firm that
/// already wears the brand, with the `brand` row already naming it, is a
/// no-op success.
pub async fn move_brand(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    brand_key: &str,
    target_firm_id: Uuid,
) -> Result<(), FirmError> {
    if crate::postgres::tenancy::enabled() {
        if actor_role != Role::Owner {
            return Err(FirmError::NotAuthorized);
        }
        return Box::pin(crate::postgres::tenancy::move_brand(
            brand_key,
            target_firm_id,
        ))
        .await;
    }
    let brand = crate::brands::find_by_key(surreal, brand_key)
        .await
        .map_err(|error| FirmError::Brand(Box::new(error)))?
        .ok_or_else(|| FirmError::UnknownBrand(brand_key.to_string()))?;
    if find_by_id(surreal, target_firm_id).await?.is_none() {
        return Err(FirmError::NoSuchFirm(target_firm_id));
    }
    authorize_brand_ownership(
        surreal,
        actor_role,
        actor_person_id,
        target_firm_id,
        crate::firm_capability::FirmCapability::MoveBrand,
    )
    .await?;
    let worn_by = firm_id_for_brand_key(surreal, brand_key).await?;
    if worn_by != Some(target_firm_id) || brand.firm_id != Some(target_firm_id) {
        let now = chrono::Utc::now().to_rfc3339();
        retry::writing(|| {
            surreal
                .query(format!(
                    "BEGIN; \
                     DELETE {BRAND_TABLE} WHERE brand_key = $brand_key; \
                     UPDATE $brand SET firm_id = $firm_id, updated_at = $now; \
                     CREATE $seat SET firm_id = $firm_id, brand_key = $brand_key, \
                     inserted_at = $now, updated_at = $now; \
                     COMMIT;"
                ))
                .bind(("brand_key", brand_key.to_string()))
                .bind(("brand", record_id(crate::brands::TABLE, brand.id)))
                .bind(("firm_id", record_id(TABLE, target_firm_id)))
                .bind(("now", now.clone()))
                .bind(("seat", record_id(BRAND_TABLE, Uuid::now_v7())))
        })
        .await?;
    }
    // The target may predate optional mirroring. An identical move also retries
    // projections after a prior mirror failure without rewriting source timestamps.
    // Keep parent-before-child order and the best-effort mirror contract.
    crate::postgres_mirror::after_writes(
        surreal,
        [
            MirrorKey::Firm(target_firm_id),
            MirrorKey::Brand(brand.id),
            MirrorKey::FirmBrand {
                brand_key: brand_key.to_string(),
            },
        ],
    )
    .await;
    Ok(())
}

/// Assign each unowned Project to the Firm wearing its brand, or to the
/// deployment's anchor Firm when no Firm wears that brand. Idempotent.
pub async fn backfill_unowned_projects(
    surreal: &SurrealDb,
    fallback_firm_id: Option<Uuid>,
) -> Result<u64, FirmError> {
    if let Some(firm_id) = fallback_firm_id {
        if find_by_id(surreal, firm_id).await?.is_none() {
            return Err(FirmError::NoSuchFirm(firm_id));
        }
    }
    if crate::postgres::tenancy::enabled() {
        return Ok(
            Box::pin(crate::postgres::tenancy::backfill_unowned_projects(
                fallback_firm_id,
            ))
            .await?,
        );
    }
    let mut response = surreal
        .query("SELECT id, brand FROM project WHERE firm_id IS NONE")
        .await?;
    let rows: Vec<TouchedProject> = response.take(0)?;
    let mut firms_by_brand = HashMap::new();
    let mut assigned = 0;
    for row in rows {
        let brand_firm = if let Some(firm_id) = firms_by_brand.get(&row.brand) {
            *firm_id
        } else {
            let firm_id = firm_id_for_brand_key(surreal, &row.brand).await?;
            firms_by_brand.insert(row.brand.clone(), firm_id);
            firm_id
        };
        let Some(firm_id) = brand_firm.or(fallback_firm_id) else {
            continue;
        };
        let mut response = writing(|| {
            surreal
                .query("UPDATE $id SET firm_id = $firm_id WHERE firm_id IS NONE RETURN AFTER")
                .bind(("id", row.id.clone()))
                .bind(("firm_id", record_id(TABLE, firm_id)))
        })
        .await?;
        let touched: Vec<ProjectIdRow> = response.take(0).unwrap_or_default();
        assigned += touched.len() as u64;
        crate::postgres_mirror::after_writes(
            surreal,
            touched
                .iter()
                .filter_map(|project| record_uuid(&project.id))
                .map(MirrorKey::Project),
        )
        .await;
    }
    Ok(assigned)
}

/// Grant membership when missing. A duplicate pair is a no-op.
pub async fn ensure_membership(
    surreal: &SurrealDb,
    input: &NewPersonFirmRole,
) -> Result<(), FirmError> {
    match add_membership(surreal, input).await {
        Ok(_) | Err(FirmError::DuplicateMembership) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Appoint or transfer a Firm's Admin DRI (ENG-499).
///
/// Owner-only: gated through
/// [`crate::firm_capability::FirmCapability::ManageAdminDri`], which no
/// membership tier admits, so an Admin — even the Firm's own — is refused
/// with [`FirmError::NotAuthorized`]. The proposed DRI must carry
/// `person.role = admin` or `person.role = owner`
/// ([`FirmError::IneligibleAdminDriTier`]) and hold an `admin` membership on
/// this exact Firm: a row here carrying a different membership is
/// [`FirmError::WrongAdminDriMembership`]. An Admin with no row on this Firm
/// at all is [`FirmError::AdminDriCrossFirm`]; an Owner with no row is
/// granted the `admin` membership in the same transaction, since Owner is
/// deployment-wide and already governs every Firm.
///
/// The transfer itself is one transaction — clearing any current DRI on this
/// Firm and setting the new one — so a concurrent reader never observes zero
/// or two Admin DRIs on the same Firm.
pub async fn appoint_admin_dri(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    person_id: Uuid,
) -> Result<PersonFirmRole, FirmError> {
    let result = if crate::postgres::tenancy::enabled() {
        Box::pin(crate::postgres::tenancy::appoint_admin_dri(
            actor_role, firm_id, person_id,
        ))
        .await
    } else {
        appoint_admin_dri_inner(surreal, actor_role, actor_person_id, firm_id, person_id).await
    };
    // ENG-464: the appointment/transfer outcome, distinct from
    // `firm_capability.resolve`'s own event above it — this names *who was
    // appointed* and *why it did or did not happen*, ids only.
    let reason = match &result {
        Ok(_) => "appointed",
        Err(FirmError::NotAuthorized) => "not_authorized",
        Err(FirmError::NoSuchFirm(_)) => "no_such_firm",
        Err(FirmError::NoSuchPerson(_)) => "no_such_person",
        Err(FirmError::IneligibleAdminDriTier(_)) => "ineligible_tier",
        Err(FirmError::AdminDriCrossFirm(_, _)) => "cross_firm",
        Err(FirmError::WrongAdminDriMembership(_, _)) => "wrong_membership",
        Err(_) => "error",
    };
    tracing::info!(
        target: "firm.appoint_admin_dri",
        firm_id = %firm_id,
        person_id = %person_id,
        actor_person_id = actor_person_id.map(|id| id.to_string()),
        succeeded = result.is_ok(),
        reason,
        "admin DRI appointment/transfer",
    );
    result
}

async fn appoint_admin_dri_inner(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    person_id: Uuid,
) -> Result<PersonFirmRole, FirmError> {
    match crate::firm_capability::resolve(
        surreal,
        actor_role,
        actor_person_id,
        firm_id,
        crate::firm_capability::FirmCapability::ManageAdminDri,
    )
    .await?
    {
        crate::firm_capability::FirmCapabilityDecision::Allowed => {}
        crate::firm_capability::FirmCapabilityDecision::FirmNotFound => {
            return Err(FirmError::NoSuchFirm(firm_id));
        }
        crate::firm_capability::FirmCapabilityDecision::Forbidden => {
            return Err(FirmError::NotAuthorized);
        }
    }

    let person = crate::persons::find_by_id(surreal, person_id)
        .await?
        .ok_or(FirmError::NoSuchPerson(person_id))?;
    if !person.role.is_admin_tier() {
        return Err(FirmError::IneligibleAdminDriTier(person_id));
    }
    let grant_owner_membership = match membership_for_person(surreal, person_id, firm_id).await? {
        None if person.role.is_owner() => true,
        None => return Err(FirmError::AdminDriCrossFirm(person_id, firm_id)),
        Some(row) if row.membership != FirmMembership::Admin => {
            return Err(FirmError::WrongAdminDriMembership(person_id, firm_id));
        }
        Some(_) => false,
    };
    let set_dri = if grant_owner_membership {
        "CREATE $membership_id SET person_id = $person_id, firm_id = $firm_id, \
         membership = 'admin', is_dri = true, inserted_at = $now, updated_at = $now;"
    } else {
        "UPDATE person_firm_role SET is_dri = true, updated_at = $now \
         WHERE person_id = $person_id AND firm_id = $firm_id;"
    };

    let membership_id = Uuid::now_v7();
    let now = chrono::Utc::now().to_rfc3339();
    writing(|| {
        surreal
            .query(format!(
                "BEGIN; \
                 UPDATE person_firm_role SET is_dri = false, updated_at = $now \
                 WHERE firm_id = $firm_id AND is_dri = true; \
                 {set_dri} \
                 COMMIT;"
            ))
            .bind(("firm_id", record_id(TABLE, firm_id)))
            .bind(("person_id", record_id(PERSON_TABLE, person_id)))
            .bind(("membership_id", record_id(MEMBERSHIP_TABLE, membership_id)))
            .bind(("now", now.clone()))
    })
    .await?;
    if crate::postgres_mirror::enabled() {
        let memberships = memberships_for_firm(surreal, firm_id).await?;
        crate::postgres_mirror::after_writes(
            surreal,
            memberships
                .into_iter()
                .map(|role| MirrorKey::FirmMembership {
                    person_id: role.person_id,
                    firm_id,
                }),
        )
        .await;
    }

    membership_for_person(surreal, person_id, firm_id)
        .await?
        .ok_or(FirmError::WriteReturnedNothing)
}

/// Refuse a `person_firm_role` edit that would leave an active Firm with no
/// Admin DRI (ENG-499 scope item 3).
///
/// Called by [`crate::firms`]'s own membership-removal doors before they
/// delete a row or change its `membership` away from `admin`.
/// `new_membership` is `None` for a removal and `Some(m)` for an update to
/// `m`; a no-op when the row is not the Firm's current DRI, or the Firm is
/// not active. Transferring the designation first with
/// [`appoint_admin_dri`] is the only way past this guard for the current
/// sole DRI.
pub async fn refuse_admin_dri_orphaning(
    surreal: &SurrealDb,
    firm_id: Uuid,
    person_id: Uuid,
    new_membership: Option<FirmMembership>,
) -> Result<(), FirmError> {
    if new_membership == Some(FirmMembership::Admin) {
        return Ok(());
    }
    let Some(row) = membership_for_person(surreal, person_id, firm_id).await? else {
        return Ok(());
    };
    if !row.is_dri {
        return Ok(());
    }
    let Some(firm) = find_by_id(surreal, firm_id).await? else {
        return Ok(());
    };
    if firm.status != "active" {
        return Ok(());
    }
    Err(FirmError::WouldLeaveFirmWithoutAdminDri(firm_id))
}

/// One active Firm's Admin-DRI standing, for the deployment-wide invariant
/// report ([`admin_dri_invariant_report`]). Every active Firm reports
/// `problem: None` when exactly one eligible Admin DRI holds the
/// designation — anything else is an actionable state a human resolves,
/// never one this report repairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminDriStatus {
    pub firm_id: Uuid,
    pub firm_name: String,
    pub problem: Option<AdminDriProblem>,
}

/// Why a Firm's Admin-DRI designation is not the invariant ENG-499 requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminDriProblem {
    /// No `person_firm_role` row on this Firm carries `is_dri = true`.
    Missing,
    /// More than one row does. Carries every such person's id.
    Multiple(Vec<Uuid>),
    /// Exactly one row carries `is_dri = true`, but the person it names no
    /// longer carries `person.role = admin` or `owner` (or the row's own membership has
    /// drifted off `admin`) — a designation that was valid at appointment
    /// and is not any more.
    Ineligible(Uuid),
}

/// Scan every active Firm and report which ones do not hold exactly one
/// eligible Admin DRI. Read-only: it never appoints, clears, or otherwise
/// repairs a row — [`appoint_admin_dri`] is the only writer.
pub async fn admin_dri_invariant_report(
    surreal: &SurrealDb,
) -> Result<Vec<AdminDriStatus>, FirmError> {
    let mut out = Vec::new();
    for firm in all(surreal).await? {
        if firm.status != "active" {
            continue;
        }
        let dris: Vec<PersonFirmRole> = memberships_for_firm(surreal, firm.id)
            .await?
            .into_iter()
            .filter(|role| role.is_dri)
            .collect();
        let problem = if dris.is_empty() {
            Some(AdminDriProblem::Missing)
        } else if dris.len() > 1 {
            Some(AdminDriProblem::Multiple(
                dris.iter().map(|row| row.person_id).collect(),
            ))
        } else {
            let dri = &dris[0];
            if dri.membership == FirmMembership::Admin {
                match crate::persons::find_by_id(surreal, dri.person_id).await? {
                    Some(person) if person.role.is_admin_tier() => None,
                    _ => Some(AdminDriProblem::Ineligible(dri.person_id)),
                }
            } else {
                Some(AdminDriProblem::Ineligible(dri.person_id))
            }
        };
        out.push(AdminDriStatus {
            firm_id: firm.id,
            firm_name: firm.name.clone(),
            problem,
        });
    }
    Ok(out)
}

/// The deployment's anchor Firm — the practice wearing the anchor Entity
/// (`NAVIGATOR_BOOTSTRAP_COMPANY`, or the shipped [`crate::seed::FIRM_ENTITY_NAME`]
/// when unset) — or `None` when no Firm row wraps that Entity yet.
///
/// ENG-495: this is the default a newly created Lawyer or Clerk joins when no
/// other Firm is named. A deployment holding exactly one Firm degenerates to
/// that Firm, which is today's single-practice answer; a deployment holding
/// several still resolves deterministically, because the anchor Entity is
/// unique by construction (`entity_firm_anchor`).
///
/// Reads the same environment variable
/// [`crate::seed::BOOTSTRAP_COMPANY_ENV`] `portal::admin::bootstrap_company_from_env`
/// resolves for the Entity surface, so the two agree on which Entity is the
/// anchor without either importing the other.
pub async fn anchor_firm(surreal: &SurrealDb) -> Result<Option<Firm>, FirmError> {
    let configured = std::env::var(crate::seed::BOOTSTRAP_COMPANY_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::seed::FIRM_ENTITY_NAME.to_string());
    // The stored `firm_anchor` key is the anchor Entity's own name, lowercased
    // — never the configured string directly (`entity_commands::firm_anchor_key`).
    // The two are guaranteed to match: `is_firm_anchor` only let that Entity
    // claim the key by matching `configured` case-insensitively in the first
    // place, so the lowercased forms are identical.
    let key = configured.to_lowercase();
    let Some(entity_id) = crate::entities::firm_anchor_holder(surreal, &key).await? else {
        return Ok(None);
    };
    find_by_entity_id(surreal, entity_id).await
}

#[cfg(test)]
mod calendar_tests;

#[cfg(test)]
#[path = "firms/compatibility_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "firms/native_tests.rs"]
mod native_tests;
