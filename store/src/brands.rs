//! Data-driven brand rows (ENG-496, ENG-659).
//!
//! A [`Brand`] is a name a practice presents under, distinct from the
//! compiled [`views::brand::BrandKey`] registry `store` cannot depend on:
//! that enum still owns every served host, the marketing-page catalog, and
//! the compiled `Branding` copy — nothing here replaces it, and no runtime
//! brand row publishes a host or a marketing page in this cut. This table
//! is the authorization and identity record CRUD acts on: who may create a
//! brand, what it is named, and which Firm it belongs to.
//!
//! Every brand row is Firm-scoped (ENG-659): `firm_id` is required, and
//! [`create`] refuses `firm_id: None` before anything else, for every actor
//! including Owner. Only a Firm's own Admin DRI (`person_firm_role.is_dri`,
//! ENG-499) may create that Firm's brand. `is_law_firm` and `legal_entity`
//! are not caller input at all — a Firm-scoped brand always inherits
//! `is_law_firm = true` and its Firm's own Entity name, because a
//! Firm-scoped brand presents that Firm's own practice. Owner still governs
//! *existing* brands on every Firm ([`authorize_existing`]/[`update`]) — the
//! restriction is on creating a new row, not on editing one.
//!
//! A `firm_id IS NONE` row is a historical fact only: every compiled house
//! brand used to migrate into the table this way, before ENG-659's schema
//! migration backfilled each one onto the Firm that wears it
//! (`store::schema::backfill_brand_firm_id`) and tightened the column to a
//! required `record<firm>`. [`system_wide`] and [`all_firm_scoped`] stay
//! read-only historical-compatibility helpers; nothing can write a new
//! `firm_id: None` row through this module.

use serde::Serialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::persons::Role;

/// The closed list of open-source licences a brand's uploaded font may be
/// attested under (ENG-586). Matches the schema `ASSERT` on
/// `brand.font_licence`; this is the friendlier Rust-side refusal before a
/// bad value ever reaches the database.
pub const FONT_LICENCES: &[&str] = &["OFL-1.1", "Apache-2.0", "UFL-1.0"];

const WHITE: [u8; 3] = [0xff, 0xff, 0xff];
const BLACK: [u8; 3] = [0x00, 0x00, 0x00];

/// The light-mode page surface every brand's primary colour actually
/// renders against — `--nav-color-bg: #ffffff` in
/// `server/public/css/tokens.css`, the same default
/// `views::brand_presentation::scheme_bg` falls back to for a scheme with no
/// explicit `bg`. Duplicated as a literal for the reason [`parse_hex`]'s doc
/// comment gives: `store` does not depend on `views` or the CSS.
const LIGHT_PAGE_SURFACE: [u8; 3] = WHITE;

/// WCAG AA contrast floor for a brand's primary colour against its
/// deterministically-chosen on-primary text colour (ENG-629).
const MIN_ON_PRIMARY_CONTRAST: f64 = 4.5;

/// WCAG UI-component contrast floor (1.4.11) for a brand's primary colour
/// against the light page surface it renders on (ENG-629).
const MIN_SURFACE_CONTRAST: f64 = 3.0;

/// Parse `#rrggbb` into sRGB bytes. A self-contained copy of
/// `views::brand_presentation::parse_hex` — `store` does not depend on
/// `views`, so the small, pure colour-math primitives this validation needs
/// are duplicated rather than imported (the same reasoning
/// `store::firms::CLOSED_BRAND_KEYS` gives for its own copy of `BrandKey::ALL`).
fn parse_hex(hex: &str) -> Option<[u8; 3]> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let byte = |offset: usize| u8::from_str_radix(&hex[offset..offset + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

fn channel_luminance(value: u8) -> f64 {
    let c = f64::from(value) / 255.0;
    if c <= 0.039_28 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn relative_luminance([r, g, b]: [u8; 3]) -> f64 {
    0.2126 * channel_luminance(r) + 0.7152 * channel_luminance(g) + 0.0722 * channel_luminance(b)
}

fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (l1, l2) = (relative_luminance(a), relative_luminance(b));
    let (lighter, darker) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Validate a brand's proposed primary hex against the two fixed
/// backgrounds it actually renders on (ENG-629): well-formed `#rrggbb`
/// first, then its deterministically-chosen on-primary text colour — white
/// or black, whichever contrasts more, never "the best of both" as a single
/// opaque ratio — must clear WCAG AA 4.5:1, and the primary itself must
/// clear 3:1 against [`LIGHT_PAGE_SURFACE`]. Refuses before any write,
/// naming which background failed and the ratio so the caller can state it.
///
/// The on-primary check alone cannot reject a well-formed hex:
/// `max(contrast(hex, white), contrast(hex, black))` has a mathematical
/// floor of `sqrt(1.05 * 0.05) / 0.05 ≈ 4.58`, reached only at the exact
/// luminance where the two are equal — every other value clears 4.5:1 by
/// more. The page-surface check is what can actually refuse an
/// ill-considered brand colour: a pale or low-saturation primary can read
/// fine against whichever on-primary text it picks while all but
/// disappearing against the white page around it.
/// `create_and_update_refuse_a_pale_primary_that_would_clear_the_old_best_of_gate`
/// proves this failure mode; the former (ENG-586) test proved only that the
/// old best-of check could never reject anything.
fn validate_primary_hex(value: &str) -> Result<(), BrandError> {
    let rgb = parse_hex(value).ok_or_else(|| BrandError::InvalidHex(value.to_string()))?;

    let (on_primary, against) = if contrast_ratio(rgb, WHITE) >= contrast_ratio(rgb, BLACK) {
        (WHITE, "white on-primary text")
    } else {
        (BLACK, "black on-primary text")
    };
    let text_ratio = contrast_ratio(rgb, on_primary);
    if text_ratio < MIN_ON_PRIMARY_CONTRAST {
        return Err(BrandError::InsufficientContrast {
            hex: value.to_string(),
            against,
            ratio: text_ratio,
            required: MIN_ON_PRIMARY_CONTRAST,
        });
    }

    let surface_ratio = contrast_ratio(rgb, LIGHT_PAGE_SURFACE);
    if surface_ratio < MIN_SURFACE_CONTRAST {
        return Err(BrandError::InsufficientContrast {
            hex: value.to_string(),
            against: "the light page surface",
            ratio: surface_ratio,
            required: MIN_SURFACE_CONTRAST,
        });
    }

    Ok(())
}

/// A data-driven brand row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Brand {
    pub id: Uuid,
    pub name: String,
    pub key: String,
    /// `None` is system-wide; `Some(firm_id)` is scoped to that Firm.
    pub firm_id: Option<Uuid>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub typeface: Option<String>,
    pub is_law_firm: bool,
    pub legal_entity: Option<String>,
    /// The uploaded logo's public-bucket object key, when one exists.
    pub logo_object_key: Option<String>,
    /// The uploaded logo's validated content type (`image/png` or
    /// `image/svg+xml`).
    pub logo_content_type: Option<String>,
    /// The uploaded font's CSS `font-family` name, when `typeface` is
    /// `"uploaded"`.
    pub font_family: Option<String>,
    /// The uploaded `.woff2`'s public-bucket object key.
    pub font_object_key: Option<String>,
    /// The attested open licence the uploaded font is under. See
    /// [`FONT_LICENCES`].
    pub font_licence: Option<String>,
    /// The Brand's canonical website, `https://<host>`, when the Owner has
    /// approved one. Its host is the only source of the Brand's approved
    /// email domain ([`crate::brand_domains`]).
    pub website_url: Option<String>,
    pub inserted_at: String,
    pub updated_at: String,
}

/// Inputs for creating a [`Brand`]. `firm_id` is required — [`create`]
/// refuses `None` before anything else (ENG-659). `is_law_firm` and
/// `legal_entity` are not caller input at all: every brand is always
/// computed as `is_law_firm = true` with its target Firm's own Entity name.
#[derive(Debug, Clone, Default)]
pub struct NewBrand {
    pub name: String,
    pub key: String,
    pub firm_id: Option<Uuid>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub typeface: Option<String>,
}

/// A partial edit to a [`Brand`]'s presentation fields. Never touches
/// `firm_id`, `is_law_firm`, or `legal_entity` — those are fixed at
/// creation and inherited, not edited. Never touches the logo or font
/// object-storage fields either — [`set_logo`] and [`set_font`] are the only
/// writers for those, since only a native multipart upload can put bytes in
/// the object matching them.
#[derive(Debug, Clone, Default)]
pub struct BrandEdit {
    pub name: Option<String>,
    /// A validated `#rrggbb` hex (ENG-586), gated behind
    /// [`validate_primary_hex`]. `Some(None)` clears it (falls back to the
    /// compiled default); `Some(Some(hex))` sets it.
    pub primary_color: Option<Option<String>>,
    pub accent_color: Option<Option<String>>,
    pub typeface: Option<Option<String>>,
    /// The CSS `font-family` name for an already-uploaded font. Renaming it
    /// does not touch the uploaded object — see [`set_font`].
    pub font_family: Option<Option<String>>,
}

/// Errors from the brand command seam.
#[derive(Debug, thiserror::Error)]
pub enum BrandError {
    #[error(transparent)]
    Postgres(#[from] crate::postgres::tenancy::TenancyError),
    #[error("PostgreSQL brand operation failed")]
    PostgresSql(#[from] sqlx::Error),
    #[error(transparent)]
    Firm(#[from] crate::firms::FirmError),
    #[error("writing a brand returned no usable row")]
    WriteReturnedNothing,
    #[error("no brand {0}")]
    NoSuchBrand(Uuid),
    #[error("no firm {0}")]
    NoSuchFirm(Uuid),
    #[error("that brand name is already taken")]
    DuplicateName,
    #[error("that brand key is already taken")]
    DuplicateKey,
    /// Only a Firm's own Admin DRI may create that Firm's brand. Every other
    /// actor, and an Admin DRI naming a different Firm, is refused this.
    #[error("you may not create, edit, or delete this brand")]
    NotAuthorized,
    /// [`create`] was called with `firm_id: None` (ENG-659). Every brand is
    /// Firm-scoped now — there is no system-wide brand an actor, Owner
    /// included, may create through this command.
    #[error("a brand must belong to a Firm")]
    FirmRequired,
    /// The proposed `primary_color` is not a well-formed `#rrggbb` hex.
    #[error("{0} is not a valid #rrggbb hex colour")]
    InvalidHex(String),
    /// The proposed `primary_color` fails one of the two fixed-background
    /// contrast checks (ENG-629): its deterministically-chosen on-primary
    /// text colour, or the light page surface it renders on. `against`
    /// names which one.
    #[error("{hex} is {ratio:.1}:1 against {against}; it must be at least {required:.1}:1")]
    InsufficientContrast {
        hex: String,
        against: &'static str,
        ratio: f64,
        required: f64,
    },
    /// The proposed font licence is not one of [`FONT_LICENCES`].
    #[error("font licence must be one of {}", FONT_LICENCES.join(", "))]
    InvalidFontLicence(String),
    /// The proposed website URL breaks a [`crate::brand_domains`] rule.
    #[error(transparent)]
    InvalidWebsiteUrl(#[from] crate::brand_domains::WebsiteUrlError),
    /// [`delete`] is refused while a `firm_brand` or `project.brand` row
    /// still names this brand's key — deleting it would leave those rows
    /// pointing at nothing.
    #[error("that brand is still worn by a firm or named by a project")]
    StillReferenced,
}

impl BrandError {
    /// The message to show a human on the brand create/edit/delete forms.
    /// Kept here so every adapter renders the same wording per refusal, the
    /// same convention [`crate::firms::FirmError::user_message`] follows.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::InvalidHex(hex) => format!("{hex} is not a valid #rrggbb hex colour."),
            Self::InsufficientContrast {
                hex,
                against,
                ratio,
                required,
            } => format!(
                "{hex} is {ratio:.1}:1 against {against}; it must be at least {required:.1}:1."
            ),
            Self::InvalidWebsiteUrl(error) => format!("{}.", capitalized(&error.to_string())),
            Self::InvalidFontLicence(_) => {
                format!("Pick a font licence: {}.", FONT_LICENCES.join(", "))
            }
            Self::StillReferenced => {
                "That brand is still worn by a firm or named by a project.".to_string()
            }
            Self::NotAuthorized => "You may not create, edit, or delete this brand.".to_string(),
            Self::FirmRequired => "A brand must belong to a Firm.".to_string(),
            Self::NoSuchBrand(_) => "That brand could not be found.".to_string(),
            Self::NoSuchFirm(_) => "That firm could not be found.".to_string(),
            Self::DuplicateName => "That brand name is already taken.".to_string(),
            Self::DuplicateKey => "That brand key is already taken.".to_string(),
            Self::Postgres(_)
            | Self::PostgresSql(_)
            | Self::Firm(_)
            | Self::WriteReturnedNothing => "Could not save brand.".to_string(),
        }
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Create a brand. See the module doc for the authorization rule and what a
/// Firm-scoped request inherits. `actor_role` is accepted for parity with
/// every other command in this module even though [`authorize`] no longer
/// branches on it — a Firm-scoped create is an Admin-DRI-only act now,
/// whatever the caller's role.
pub async fn create(
    _application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    input: &NewBrand,
) -> Result<Brand, BrandError> {
    if let Some(hex) = input.primary_color.as_deref() {
        validate_primary_hex(hex)?;
    }
    Box::pin(crate::postgres::tenancy::create_brand(
        actor_role,
        actor_person_id,
        input,
    ))
    .await
}

/// System/seed-only: create a compiled Brand or refresh its presentation.
/// Under PostgreSQL tenancy the row and its `firm_brand` seat take
/// `input.firm_id`. SurrealDB leaves an existing seat for the boot seed,
/// which returns the brand to the firm that seed names. An existing website
/// URL stays. Human and API writes use the authorized [`create`]/[`update`]
/// paths, which never let `firm_id` move.
pub(crate) async fn seed_upsert(
    _application_store: &ApplicationStore,
    input: &NewBrand,
    website_url: Option<&str>,
) -> Result<Brand, BrandError> {
    Box::pin(crate::postgres::tenancy::seed_brand(input, website_url)).await
}

/// Find a brand by id.
pub async fn find_by_id(
    _application_store: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Brand>, BrandError> {
    Ok(Box::pin(crate::postgres::tenancy::find_brand(id)).await?)
}

/// Find a brand by its unique key.
pub async fn find_by_key(
    _application_store: &ApplicationStore,
    key: &str,
) -> Result<Option<Brand>, BrandError> {
    Ok(Box::pin(crate::postgres::tenancy::brand_by_key(key)).await?)
}

/// Find a brand and resolve the caller's Firm capability before a caller uses
/// the row for a Firm-scoped read or command. System-wide brands remain
/// Owner-only; Firm-scoped brands go through [`crate::firm_capability::resolve`]
/// with [`crate::firm_capability::FirmCapability::ManageBrand`]. Missing and
/// forbidden targets stay typed so each response boundary can render the same
/// not-found answer without disclosing another Firm's row.
pub async fn find_by_key_for_actor(
    _application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    key: &str,
) -> Result<Brand, BrandError> {
    Box::pin(crate::postgres::tenancy::brand_for_actor(
        actor_role,
        actor_person_id,
        key,
    ))
    .await
}

/// Every system-wide brand (`firm_id IS NONE`), name then id — the set
/// every Firm sees regardless of its own scoped brands.
pub async fn system_wide(_application_store: &ApplicationStore) -> Result<Vec<Brand>, BrandError> {
    Ok(Box::pin(crate::postgres::tenancy::system_brands()).await?)
}

/// Every Firm-scoped brand across the whole deployment, name then id — the
/// Owner inventory [`visible_for_actor`] lists alongside [`system_wide`]. Never
/// used for a Firm-scoped viewer's own listing, which stays [`for_firm`].
pub async fn all_firm_scoped(
    _application_store: &ApplicationStore,
) -> Result<Vec<Brand>, BrandError> {
    Ok(Box::pin(crate::postgres::tenancy::all_firm_brands()).await?)
}

/// Brands the actor may see on `/app/admin/brands`.
///
/// System-wide rows are visible to every Firm. Owner also sees every
/// Firm-scoped row. An Admin sees only the Firm-scoped rows they hold
/// [`crate::firm_capability::FirmCapability::ManageBrand`] on — that Firm's
/// Admin DRI — so one practice's administrator cannot read another's
/// presentation inventory.
pub async fn visible_for_actor(
    _application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
) -> Result<Vec<Brand>, BrandError> {
    Ok(Box::pin(crate::postgres::tenancy::visible_brands_for_actor(
        actor_role,
        actor_person_id,
    ))
    .await?)
}

/// Every brand scoped to this Firm — never another Firm's, and never the
/// system-wide set (use [`system_wide`] for that).
pub async fn for_firm(
    _application_store: &ApplicationStore,
    firm_id: Uuid,
) -> Result<Vec<Brand>, BrandError> {
    Ok(Box::pin(crate::postgres::tenancy::brands_for_firm(firm_id)).await?)
}

/// Edit a brand's presentation fields. Owner governs existing brands on every
/// Firm; a Firm's own Admin DRI governs its Firm-scoped one.
pub async fn update(
    _application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    brand_id: Uuid,
    input: &BrandEdit,
) -> Result<Brand, BrandError> {
    if let Some(Some(hex)) = input.primary_color.as_ref() {
        validate_primary_hex(hex)?;
    }
    Box::pin(crate::postgres::tenancy::update_brand(
        actor_role,
        actor_person_id,
        brand_id,
        input,
    ))
    .await
}

/// Attach an uploaded logo. Authorized exactly as [`update`]. `object_key` is
/// the public assets-bucket key the caller already wrote the validated bytes
/// to; `content_type` is `image/png` or `image/svg+xml`. Overwrites any prior
/// logo — the caller is responsible for deleting the superseded object if it
/// wants the bucket tidy.
pub async fn set_logo(
    _application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    brand_id: Uuid,
    object_key: &str,
    content_type: &str,
) -> Result<Brand, BrandError> {
    Box::pin(crate::postgres::tenancy::set_brand_logo(
        actor_role,
        actor_person_id,
        brand_id,
        object_key,
        content_type,
    ))
    .await
}

/// Attach an uploaded font. Authorized exactly as [`update`]. `licence` must
/// be one of [`FONT_LICENCES`] — refused before any write otherwise, the
/// friendlier twin of the schema's own `ASSERT`. Setting `typeface` to
/// `"uploaded"` is the caller's job (the presentational form field), not this
/// function's — a brand may upload a font and keep wearing a catalog
/// typeface until it switches over.
pub async fn set_font(
    _application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    brand_id: Uuid,
    family: &str,
    object_key: &str,
    licence: &str,
) -> Result<Brand, BrandError> {
    if !FONT_LICENCES.contains(&licence) {
        return Err(BrandError::InvalidFontLicence(licence.to_string()));
    }
    Box::pin(crate::postgres::tenancy::set_brand_font(
        actor_role,
        actor_person_id,
        brand_id,
        family,
        object_key,
        licence,
    ))
    .await
}

/// Approve or clear a Brand's canonical website URL. Owner only: the URL's
/// host becomes an approved email domain for account admission
/// ([`crate::brand_domains`]), so this approval is kept apart from Brand
/// ownership — a Firm's Admin DRI, who governs the rest of the row, cannot
/// widen admission. The URL is canonicalized before it is stored; `None`
/// clears it, which removes the Brand's domain from the policy.
pub async fn set_website_url(
    _application_store: &ApplicationStore,
    actor_role: Role,
    brand_id: Uuid,
    website_url: Option<&str>,
) -> Result<Brand, BrandError> {
    if actor_role != Role::Owner {
        return Err(BrandError::NotAuthorized);
    }
    Box::pin(crate::postgres::tenancy::set_brand_website(
        brand_id,
        website_url,
    ))
    .await
}

/// Delete a brand. Authorized exactly as [`update`]. Refused with
/// [`BrandError::StillReferenced`] while any `firm_brand` or `project.brand`
/// row still names this brand's key — deleting it would orphan those rows.
/// Deleting an unworn, unnamed brand removes only the row; its uploaded
/// logo/font objects in the public assets bucket are the caller's to clean up
/// (this function has no bucket handle).
pub async fn delete(
    _application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    brand_id: Uuid,
) -> Result<(), BrandError> {
    Box::pin(crate::postgres::tenancy::delete_brand(
        actor_role,
        actor_person_id,
        brand_id,
    ))
    .await
}

#[cfg(test)]
mod tests {}
