//! The letterhead a [Brand](crate::brands) frames its letters with (LAW-266).
//!
//! One `brand_letterhead` row per Brand, born in PostgreSQL with no Surreal
//! source, keyed by the Brand's id. An Owner, or the Admin DRI of the Brand's
//! own Firm, edits it on the Brand's edit page; [`save_for_actor`] carries that
//! gate by resolving the Brand through [`crate::brands::find_by_key_for_actor`]
//! before anything is written.
//!
//! The stored row ([`LetterheadFields`]) holds only what an admin set. The
//! renderer consumes [`BrandLetterhead`], which [`resolve`] assembles from the
//! row and the Brand itself: an unset wordmark is the Brand's name, an unset
//! site is the Brand's approved website, an unset email is `contact@` that
//! website's registrable domain, and the Firm's legal name is always the
//! Brand's own `legal_entity`. [`for_brand_key`] is the one read every
//! letterhead renderer uses.
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::brands::Brand;
use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub enum LetterheadError {
    /// A field failed validation; the message names it for the form.
    #[error("{0}")]
    Invalid(String),
    /// The Brand does not exist, or the actor may not edit it. One variant so
    /// a caller answers both with the same non-disclosing 404.
    #[error("no such brand")]
    NotFound,
    #[error("brand lookup failed: {0}")]
    Brand(#[from] crate::brands::BrandError),
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

/// Which mark heads the letterhead: the Brand's wordmark in letterspaced
/// capitals, or its uploaded logo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LetterheadMark {
    #[default]
    Wordmark,
    Logo,
}

impl LetterheadMark {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wordmark => "wordmark",
            Self::Logo => "logo",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "wordmark" => Some(Self::Wordmark),
            "logo" => Some(Self::Logo),
            _ => None,
        }
    }
}

/// What an admin set on a Brand's letterhead. `None` means "fall back to the
/// Brand" (see [`resolve`]).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LetterheadFields {
    pub wordmark: Option<String>,
    #[serde(default)]
    pub mark: LetterheadMark,
    pub phone: Option<String>,
    pub email: Option<String>,
    /// `https://<host>`, canonicalised by
    /// [`crate::brand_domains::canonical_website_url`].
    pub site_url: Option<String>,
    /// The postal address, one line per row.
    pub postal_address: Option<String>,
    /// One of the Brand's uploaded font families.
    pub font_family: Option<String>,
}

/// The letterhead a renderer frames a letter with: every line resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandLetterhead {
    pub brand_key: String,
    /// The wordmark across the top, in natural case.
    pub wordmark: String,
    pub mark: LetterheadMark,
    /// The Brand's uploaded logo object, when it has one. A renderer asked
    /// for [`LetterheadMark::Logo`] without one prints the wordmark.
    pub logo_object_key: Option<String>,
    /// The legal name of the Firm that owns the Brand.
    pub firm_legal_name: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    /// The site as a reader types it, e.g. `www.example.com`.
    pub web: Option<String>,
    pub postal_address: Option<String>,
    /// The letterhead font family and its uploaded `.woff2` object. `None`
    /// is the renderer's own default face.
    pub font_family: Option<String>,
    pub font_object_key: Option<String>,
    /// The stored row's last edit as RFC 3339 — the revision a render's
    /// provenance names. `None` when no row was read (an in-memory
    /// preview resolve, or no admin has saved a row yet).
    pub updated_at: Option<String>,
}

fn optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Trim and validate an admin's submission against the Brand it is for.
///
/// # Errors
/// [`LetterheadError::Invalid`] naming the first field that fails.
pub fn validate(
    brand: &Brand,
    input: &LetterheadFields,
) -> Result<LetterheadFields, LetterheadError> {
    let invalid = |message: &str| Err(LetterheadError::Invalid(message.to_string()));
    let wordmark = optional(input.wordmark.as_deref());
    if wordmark
        .as_ref()
        .is_some_and(|value| value.chars().count() > 100)
    {
        return invalid("The wordmark must be at most 100 characters.");
    }
    if input.mark == LetterheadMark::Logo && brand.logo_object_key.is_none() {
        return invalid("This Brand has no uploaded logo to head its letterhead.");
    }
    let phone = optional(input.phone.as_deref());
    if phone.as_ref().is_some_and(|value| {
        value.chars().count() > 40
            || !value
                .chars()
                .all(|c| c.is_ascii_digit() || " +-().".contains(c))
            || !value.chars().any(|c| c.is_ascii_digit())
    }) {
        return invalid(
            "The phone must be digits, spaces, and + - ( ) . only, at most 40 characters.",
        );
    }
    let email = optional(input.email.as_deref());
    if email.as_ref().is_some_and(|value| {
        let Some((local, domain)) = value.split_once('@') else {
            return true;
        };
        value.len() > 254
            || local.is_empty()
            || !domain.contains('.')
            || domain.contains('@')
            || value.chars().any(char::is_whitespace)
    }) {
        return invalid("The email must be a single address, for example contact@example.com.");
    }
    let site_url = match optional(input.site_url.as_deref()) {
        None => None,
        Some(value) => Some(
            crate::brand_domains::canonical_website_url(&value).map_err(|_| {
                LetterheadError::Invalid(
                    "The site must be an https address with no path, for example https://www.example.com."
                        .to_string(),
                )
            })?,
        ),
    };
    let postal_address = optional(input.postal_address.as_deref()).map(|value| {
        value
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    });
    if postal_address
        .as_ref()
        .is_some_and(|value| value.chars().count() > 500)
    {
        return invalid("The postal address must be at most 500 characters.");
    }
    let font_family = optional(input.font_family.as_deref());
    if font_family
        .as_ref()
        .is_some_and(|value| value.chars().count() > 100)
    {
        return invalid("The letterhead font name must be at most 100 characters.");
    }
    if let Some(family) = &font_family {
        let uploaded = brand
            .font_family
            .as_deref()
            .filter(|_| brand.font_object_key.is_some());
        if uploaded != Some(family.as_str()) {
            return invalid("The letterhead font must be one of this Brand's uploaded fonts.");
        }
    }
    Ok(LetterheadFields {
        wordmark,
        mark: input.mark,
        phone,
        email,
        site_url,
        postal_address,
        font_family,
    })
}

/// The host a reader types, from a canonical `https://<host>` URL.
fn host_of(url: &str) -> Option<&str> {
    url.strip_prefix("https://")
        .map(|host| host.trim_end_matches('/'))
        .filter(|host| !host.is_empty())
}

/// Second-level labels that, under a two-letter country code, are a public
/// suffix of their own: `co.uk`, `com.au`, `ac.jp`, `org.nz`.
const COUNTRY_SECOND_LEVELS: &[&str] = &[
    "ac", "co", "com", "edu", "gen", "go", "gob", "gov", "ltd", "mil", "ne", "net", "nhs", "nom",
    "or", "org", "plc", "sch",
];

/// The registrable domain of `host`, for the default `contact@` mailbox: its
/// last two labels, or its last three when those two are a country-code
/// second level such as `co.uk` (so `www.example.co.uk` mails `contact@` at
/// `example.co.uk`, never at `co.uk`). A host that is only such a suffix has
/// no registrable domain.
///
/// This is a short list, not the public-suffix list: it covers the
/// country-code second levels a firm's site plausibly sits under, and an
/// admin whose suffix it misses sets the email outright.
fn registrable_domain(host: &str) -> Option<String> {
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|label| label.is_empty()) {
        return None;
    }
    let count = labels.len();
    let country_suffix = labels[count - 1].len() == 2
        && COUNTRY_SECOND_LEVELS.contains(&labels[count - 2].to_ascii_lowercase().as_str());
    let keep = if country_suffix { 3 } else { 2 };
    (count >= keep).then(|| labels[count - keep..].join("."))
}

/// Assemble the letterhead a renderer consumes from the Brand and its
/// stored row (absent when no admin has set one yet).
#[must_use]
pub fn resolve(brand: &Brand, stored: Option<&LetterheadFields>) -> BrandLetterhead {
    let empty = LetterheadFields::default();
    let stored = stored.unwrap_or(&empty);
    let site = stored
        .site_url
        .as_deref()
        .or(brand.website_url.as_deref())
        .and_then(host_of);
    let email = stored.email.clone().or_else(|| {
        site.and_then(registrable_domain)
            .map(|domain| format!("contact@{domain}"))
    });
    // The letterhead font is the chosen upload; with none chosen, or with a
    // choice the Brand has since replaced by another upload, the Brand's own
    // uploaded face when it wears one.
    let uploaded = brand
        .font_family
        .as_ref()
        .filter(|_| brand.font_object_key.is_some());
    let font_family = stored
        .font_family
        .clone()
        .filter(|family| uploaded == Some(family))
        .or_else(|| {
            (brand.typeface.as_deref() == Some("uploaded"))
                .then(|| brand.font_family.clone())
                .flatten()
        });
    let font_object_key = font_family
        .as_ref()
        .and_then(|family| (brand.font_family.as_ref() == Some(family)).then_some(()))
        .and_then(|()| brand.font_object_key.clone());
    BrandLetterhead {
        brand_key: brand.key.clone(),
        wordmark: stored
            .wordmark
            .clone()
            .unwrap_or_else(|| brand.name.clone()),
        mark: stored.mark,
        logo_object_key: brand.logo_object_key.clone(),
        firm_legal_name: brand.legal_entity.clone(),
        phone: stored.phone.clone(),
        email,
        web: site.map(str::to_string),
        postal_address: stored.postal_address.clone(),
        font_family,
        font_object_key,
        // Only a stored row carries a revision; the callers that read one
        // stamp it from PostgreSQL.
        updated_at: None,
    }
}

/// A stored row and its revision: the fields an admin set, and the
/// `updated_at` a render's provenance names.
struct StoredRow {
    fields: LetterheadFields,
    updated_at: chrono::DateTime<chrono::Utc>,
}

fn from_row_with_updated_at(row: &sqlx::postgres::PgRow) -> Result<StoredRow, sqlx::Error> {
    Ok(StoredRow {
        fields: from_row(row)?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<LetterheadFields, sqlx::Error> {
    let mark: String = row.try_get("mark")?;
    Ok(LetterheadFields {
        wordmark: row.try_get("wordmark")?,
        mark: LetterheadMark::parse(&mark).unwrap_or_default(),
        phone: row.try_get("phone")?,
        email: row.try_get("email")?,
        site_url: row.try_get("site_url")?,
        postal_address: row.try_get("postal_address")?,
        font_family: row.try_get("font_family")?,
    })
}

/// The stored letterhead for a Brand id, or `None` when none is set.
///
/// # Errors
/// [`LetterheadError::Database`] when the read fails.
pub async fn find(
    pg: &PostgresDb,
    brand_id: Uuid,
) -> Result<Option<LetterheadFields>, LetterheadError> {
    Ok(find_row(pg, brand_id).await?.map(|row| row.fields))
}

/// The stored row and its revision, or `None` when none is set.
async fn find_row(pg: &PostgresDb, brand_id: Uuid) -> Result<Option<StoredRow>, LetterheadError> {
    let row = sqlx::query("SELECT * FROM brand_letterhead WHERE brand_id = $1")
        .bind(brand_id)
        .fetch_optional(&pg.0)
        .await?;
    Ok(row.as_ref().map(from_row_with_updated_at).transpose()?)
}

/// Replace a Brand's stored letterhead with already-[`validate`]d fields.
/// Callers authorize first; [`save_for_actor`] is the authorized path.
///
/// # Errors
/// [`LetterheadError::Database`] when the write fails.
pub async fn put(
    pg: &PostgresDb,
    brand_id: Uuid,
    fields: &LetterheadFields,
) -> Result<LetterheadFields, LetterheadError> {
    Ok(put_row(pg, brand_id, fields).await?.fields)
}

/// [`put`], keeping the revision the write stamped.
async fn put_row(
    pg: &PostgresDb,
    brand_id: Uuid,
    fields: &LetterheadFields,
) -> Result<StoredRow, LetterheadError> {
    let row = sqlx::query(
        "INSERT INTO brand_letterhead (brand_id, wordmark, mark, phone, email, site_url, postal_address, font_family) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (brand_id) DO UPDATE SET wordmark = EXCLUDED.wordmark, mark = EXCLUDED.mark, \
         phone = EXCLUDED.phone, email = EXCLUDED.email, site_url = EXCLUDED.site_url, \
         postal_address = EXCLUDED.postal_address, font_family = EXCLUDED.font_family, updated_at = now() \
         RETURNING *",
    )
    .bind(brand_id)
    .bind(fields.wordmark.as_deref())
    .bind(fields.mark.as_str())
    .bind(fields.phone.as_deref())
    .bind(fields.email.as_deref())
    .bind(fields.site_url.as_deref())
    .bind(fields.postal_address.as_deref())
    .bind(fields.font_family.as_deref())
    .fetch_one(&pg.0)
    .await?;
    Ok(from_row_with_updated_at(&row)?)
}

/// Save a Brand's letterhead as an actor: Owner, or the Admin DRI of the
/// Brand's own Firm. Any other actor, and an unknown key, is
/// [`LetterheadError::NotFound`].
///
/// # Errors
/// [`LetterheadError::NotFound`] for an unknown or unreachable Brand,
/// [`LetterheadError::Invalid`] for a refused field, and the store errors.
pub async fn save_for_actor(
    surreal: &crate::surreal::SurrealDb,
    pg: &PostgresDb,
    actor_role: crate::persons::Role,
    actor_person_id: Option<Uuid>,
    key: &str,
    input: &LetterheadFields,
) -> Result<BrandLetterhead, LetterheadError> {
    let brand =
        match crate::brands::find_by_key_for_actor(surreal, actor_role, actor_person_id, key).await
        {
            Ok(brand) => brand,
            Err(
                crate::brands::BrandError::NotAuthorized
                | crate::brands::BrandError::NoSuchBrand(_)
                | crate::brands::BrandError::NoSuchFirm(_),
            ) => return Err(LetterheadError::NotFound),
            Err(error) => return Err(error.into()),
        };
    let fields = validate(&brand, input)?;
    let stored = put_row(pg, brand.id, &fields).await?;
    let mut resolved = resolve(&brand, Some(&stored.fields));
    resolved.updated_at = Some(stored.updated_at.to_rfc3339());
    Ok(resolved)
}

/// The resolved letterhead for a Brand key, or `None` for an unknown key —
/// the read every letterhead renderer uses.
///
/// # Errors
/// The Brand lookup and the PostgreSQL read errors.
pub async fn for_brand_key(
    surreal: &crate::surreal::SurrealDb,
    pg: &PostgresDb,
    key: &str,
) -> Result<Option<BrandLetterhead>, LetterheadError> {
    let Some(brand) = crate::brands::find_by_key(surreal, key).await? else {
        return Ok(None);
    };
    let stored = find_row(pg, brand.id).await?;
    let mut resolved = resolve(&brand, stored.as_ref().map(|row| &row.fields));
    resolved.updated_at = stored.map(|row| row.updated_at.to_rfc3339());
    Ok(Some(resolved))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brand() -> Brand {
        Brand {
            id: Uuid::from_u128(7),
            name: "Example Law".into(),
            key: "example".into(),
            firm_id: Some(Uuid::from_u128(1)),
            primary_color: None,
            accent_color: None,
            typeface: Some("uploaded".into()),
            is_law_firm: true,
            legal_entity: Some("Example Law PC".into()),
            logo_object_key: None,
            logo_content_type: None,
            font_family: Some("Example Serif".into()),
            font_object_key: Some("fonts/brands/example/abc.woff2".into()),
            font_licence: Some("OFL-1.1".into()),
            website_url: Some("https://www.example.com".into()),
            inserted_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn an_unset_letterhead_falls_back_to_the_brand() {
        let resolved = resolve(&brand(), None);
        assert_eq!(resolved.wordmark, "Example Law");
        assert_eq!(resolved.firm_legal_name.as_deref(), Some("Example Law PC"));
        assert_eq!(resolved.web.as_deref(), Some("www.example.com"));
        assert_eq!(resolved.email.as_deref(), Some("contact@example.com"));
        assert_eq!(resolved.phone, None);
        assert_eq!(resolved.postal_address, None);
        assert_eq!(resolved.font_family.as_deref(), Some("Example Serif"));
        assert_eq!(
            resolved.font_object_key.as_deref(),
            Some("fonts/brands/example/abc.woff2")
        );
        assert_eq!(resolved.mark, LetterheadMark::Wordmark);
    }

    #[test]
    fn a_brand_with_no_website_has_no_default_mailbox() {
        let mut brand = brand();
        brand.website_url = None;
        brand.typeface = Some("inter".into());
        let resolved = resolve(&brand, None);
        assert_eq!(resolved.web, None);
        assert_eq!(resolved.email, None);
        assert_eq!(
            resolved.font_family, None,
            "a catalog typeface is not an upload"
        );
    }

    #[test]
    fn stored_lines_win_and_the_default_mailbox_follows_the_stored_site() {
        let stored = LetterheadFields {
            wordmark: Some("Example".into()),
            mark: LetterheadMark::Logo,
            phone: Some("+1 555 010 0000".into()),
            email: None,
            site_url: Some("https://letters.example.org".into()),
            postal_address: Some("1 Main St\nSuite 2".into()),
            font_family: Some("Example Serif".into()),
        };
        let resolved = resolve(&brand(), Some(&stored));
        assert_eq!(resolved.wordmark, "Example");
        assert_eq!(resolved.mark, LetterheadMark::Logo);
        assert_eq!(resolved.phone.as_deref(), Some("+1 555 010 0000"));
        assert_eq!(resolved.web.as_deref(), Some("letters.example.org"));
        assert_eq!(resolved.email.as_deref(), Some("contact@example.org"));
        assert_eq!(
            resolved.postal_address.as_deref(),
            Some("1 Main St\nSuite 2")
        );
    }

    #[test]
    fn validation_trims_blanks_to_none_and_canonicalises_the_site() {
        let input = LetterheadFields {
            wordmark: Some("  ".into()),
            mark: LetterheadMark::Wordmark,
            phone: Some(" (555) 010-0000 ".into()),
            email: Some(" hello@example.com ".into()),
            site_url: Some("https://WWW.Example.com/".into()),
            postal_address: Some("  1 Main St \n\n  Springfield  ".into()),
            font_family: Some(String::new()),
        };
        let clean = validate(&brand(), &input).unwrap();
        assert_eq!(clean.wordmark, None);
        assert_eq!(clean.phone.as_deref(), Some("(555) 010-0000"));
        assert_eq!(clean.email.as_deref(), Some("hello@example.com"));
        assert_eq!(clean.site_url.as_deref(), Some("https://www.example.com"));
        assert_eq!(
            clean.postal_address.as_deref(),
            Some("1 Main St\nSpringfield")
        );
        assert_eq!(clean.font_family, None);
    }

    #[test]
    fn validation_names_the_refused_field() {
        let refused = |input: LetterheadFields| validate(&brand(), &input).unwrap_err().to_string();
        assert!(refused(LetterheadFields {
            phone: Some("call me".into()),
            ..LetterheadFields::default()
        })
        .contains("phone"));
        assert!(refused(LetterheadFields {
            email: Some("not-an-address".into()),
            ..LetterheadFields::default()
        })
        .contains("email"));
        assert!(refused(LetterheadFields {
            site_url: Some("http://www.example.com".into()),
            ..LetterheadFields::default()
        })
        .contains("site"));
        assert!(refused(LetterheadFields {
            font_family: Some("Comic Sans".into()),
            ..LetterheadFields::default()
        })
        .contains("uploaded fonts"));
        assert!(refused(LetterheadFields {
            mark: LetterheadMark::Logo,
            ..LetterheadFields::default()
        })
        .contains("no uploaded logo"));
        assert!(refused(LetterheadFields {
            wordmark: Some("w".repeat(101)),
            ..LetterheadFields::default()
        })
        .contains("wordmark"));
    }

    #[test]
    fn the_default_mailbox_keeps_a_country_code_second_level() {
        assert_eq!(
            registrable_domain("www.example.co.uk").as_deref(),
            Some("example.co.uk")
        );
        assert_eq!(
            registrable_domain("example.com.au").as_deref(),
            Some("example.com.au")
        );
        assert_eq!(
            registrable_domain("letters.example.org").as_deref(),
            Some("example.org")
        );
        assert_eq!(
            registrable_domain("example.io").as_deref(),
            Some("example.io")
        );
        assert_eq!(
            registrable_domain("co.uk"),
            None,
            "a bare suffix names no one"
        );
        let mut brand = brand();
        brand.website_url = Some("https://www.example.co.uk".into());
        let email = resolve(&brand, None).email;
        assert_eq!(
            email
                .as_deref()
                .and_then(|email| email.strip_prefix("contact@")),
            Some("example.co.uk")
        );
    }

    #[test]
    fn a_font_name_longer_than_its_column_is_refused_before_the_write() {
        let mut brand = brand();
        let long = "F".repeat(101);
        brand.font_family = Some(long.clone());
        let error = validate(
            &brand,
            &LetterheadFields {
                font_family: Some(long),
                ..LetterheadFields::default()
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("at most 100 characters"),
            "{error}"
        );
    }

    #[test]
    fn a_replaced_upload_supersedes_the_stale_stored_font() {
        let stored = LetterheadFields {
            font_family: Some("Example Serif".into()),
            ..LetterheadFields::default()
        };
        let mut brand = brand();
        brand.font_family = Some("Example Grotesk".into());
        brand.font_object_key = Some("fonts/brands/example/def.woff2".into());
        let resolved = resolve(&brand, Some(&stored));
        assert_eq!(resolved.font_family.as_deref(), Some("Example Grotesk"));
        assert_eq!(
            resolved.font_object_key.as_deref(),
            Some("fonts/brands/example/def.woff2")
        );

        brand.typeface = Some("inter".into());
        let resolved = resolve(&brand, Some(&stored));
        assert_eq!(
            resolved.font_family, None,
            "the renderer's own default face"
        );
        assert_eq!(resolved.font_object_key, None);
    }

    #[test]
    fn mark_round_trips() {
        for mark in [LetterheadMark::Wordmark, LetterheadMark::Logo] {
            assert_eq!(LetterheadMark::parse(mark.as_str()), Some(mark));
        }
        assert_eq!(LetterheadMark::parse("seal"), None);
    }
}
