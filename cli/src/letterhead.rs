//! The letterhead a Project's letter goes out under, resolved from the
//! Project's Brand (LAW-265, LAW-266).
//!
//! `project.host` in `navigator.yaml` selects the Brand from the compiled
//! registry ([`views::brand::registered_brand_key`], or the Brand's apex).
//! An optional `project.brand` must agree with that host-selected Brand.
//!
//! The frame itself comes from one of two sources. When the deployment's
//! stores are reachable, the Brand's runtime letterhead row — the same one
//! `/app/admin/brands` edits, read through
//! [`store::brand_letterheads::for_brand_key`] — supplies the wordmark,
//! the contact lines, the postal address, the letterhead font, and the
//! mark. When they are not, the compiled registry supplies the frame
//! instead, and the caller announces the fallback; see
//! `main.rs::project_letterhead`. A template re-render therefore reflects
//! the letterhead its Brand currently publishes, while executed and filed
//! artifacts keep their stored bytes (ENG-1148).
//!
//! A Brand the registry does not know is a refusal naming the key or host,
//! never the default letterhead: a letter on the wrong Brand's letterhead
//! tells its reader the wrong firm wrote it.

use std::fmt;

use store::brand_letterheads::LetterheadMark as StoreMark;
use store::brand_letterheads::{self, BrandLetterhead};
use views::brand::{brand_key_for_apex, registered_brand_key, BrandKey, DEFAULT_BRANDING};

/// Why a Project's letter has no letterhead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LetterheadError {
    /// `project.brand` names no compiled Brand.
    UnknownBrand(String),
    /// `project.host` maps to no compiled Brand.
    UnknownHost(String),
    /// `navigator.yaml` has no host to select the Brand.
    Undeclared,
    /// `project.brand` contradicts the host-selected Brand.
    BrandMismatch {
        /// The manifest's explicit Brand assertion.
        declared: BrandKey,
        /// The Brand selected by the manifest's host.
        resolved: BrandKey,
    },
    /// The compiled Brand's host cannot name a mailbox domain.
    InvalidBrandHost(BrandKey),
    /// The deployment's letterhead store could not answer.
    Store(String),
}

impl fmt::Display for LetterheadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBrand(key) => write!(
                f,
                "navigator.yaml declares `project.brand: {key}`, which names no Brand with a \
                 letterhead; correct the key before rendering a letter"
            ),
            Self::UnknownHost(host) => write!(
                f,
                "navigator.yaml's `project.host: {host}` maps to no Brand, so the letter has no \
                 letterhead; correct the host before rendering a letter"
            ),
            Self::Undeclared => write!(
                f,
                "navigator.yaml requires `project.host` to select the letterhead Brand"
            ),
            Self::BrandMismatch { declared, resolved } => write!(
                f,
                "navigator.yaml's `project.brand: {}` disagrees with the host-selected Brand \
                 `{}`; correct the declaration or host before rendering a letter",
                declared.as_str(),
                resolved.as_str()
            ),
            Self::InvalidBrandHost(key) => write!(
                f,
                "Brand `{}` has no host a letterhead mailbox can be built on",
                key.as_str()
            ),
            Self::Store(error) => {
                write!(f, "the deployment's letterhead store failed: {error}")
            }
        }
    }
}

impl std::error::Error for LetterheadError {}

/// The Brand selected by a Project's host, checked against its Brand assertion.
///
/// # Errors
/// [`LetterheadError`] naming the key or host that resolves to no Brand.
pub fn resolve_brand(brand: Option<&str>, host: Option<&str>) -> Result<BrandKey, LetterheadError> {
    fn declared(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|v| !v.is_empty())
    }
    let asserted = declared(brand)
        .map(|key| {
            BrandKey::parse(key).ok_or_else(|| LetterheadError::UnknownBrand(key.to_string()))
        })
        .transpose()?;
    let host = declared(host).ok_or(LetterheadError::Undeclared)?;
    let resolved = registered_brand_key(host)
        .or_else(|| brand_key_for_apex(host))
        .ok_or_else(|| LetterheadError::UnknownHost(host.to_string()))?;
    if let Some(declared) = asserted {
        if declared != resolved {
            return Err(LetterheadError::BrandMismatch { declared, resolved });
        }
    }
    Ok(resolved)
}

/// The letterhead `key` wears: its display name, its Firm's legal name,
/// its production host, `contact@` that host's registrable domain, and its
/// phone. Only the default Brand draws the compiled mark, which is its own.
///
/// # Errors
/// [`LetterheadError::InvalidBrandHost`] when the compiled host is not a
/// hostname, which the registry's own tests rule out.
pub fn for_brand(key: BrandKey) -> Result<pdf::Letterhead, LetterheadError> {
    let branding = key.resolve_branding(&DEFAULT_BRANDING);
    let mut letterhead = pdf::Letterhead::from_brand(
        branding.firm.site_name,
        key.canonical_host(),
        branding.firm_phone,
        "",
    )
    .map_err(|_| LetterheadError::InvalidBrandHost(key))?;
    letterhead.entity = branding.firm.legal_entity.to_string();
    letterhead.mark = if key == BrandKey::Neon {
        pdf::LetterheadMark::Compiled
    } else {
        pdf::LetterheadMark::Wordmark
    };
    Ok(letterhead)
}

/// The letterhead for a Project declaring `brand` and `host`.
///
/// # Errors
/// [`LetterheadError`] when the host has no Brand or the declaration contradicts it.
pub fn for_project(
    brand: Option<&str>,
    host: Option<&str>,
) -> Result<pdf::Letterhead, LetterheadError> {
    for_brand(resolve_brand(brand, host)?)
}

/// The frame read from the deployment's stores, with the provenance a
/// render reports beside it.
pub struct RuntimeLetterhead {
    /// The frame the letter wears.
    pub letterhead: pdf::Letterhead,
    /// The stored row's `updated_at` — the revision the render used.
    pub revision: Option<String>,
    /// The letterhead font's uploaded object, named by the provenance line.
    pub font_object_key: Option<String>,
    /// Non-fatal degradations a render announces on stderr: a mark asked
    /// for but unfetchable prints the wordmark, loudly.
    pub warnings: Vec<String>,
}

/// The runtime letterhead `key` wears, read from the deployment's stores —
/// the same rows `/app/admin/brands` edits. `None` when the deployment
/// knows no such Brand, so the caller falls back to the compiled registry
/// and says so.
///
/// The mark follows the row: `wordmark` prints the admin's wordmark, and
/// `logo` draws the Brand's uploaded mark, fetched from the deployment's
/// public assets route at `assets_base`. A logo the fetch cannot produce
/// degrades to the wordmark with a warning — never a refusal, and never
/// the compiled mark, which belongs to the default Brand alone.
///
/// # Errors
/// [`LetterheadError::Store`] when a store read fails; the caller decides
/// between refusing and falling back.
pub async fn for_brand_runtime(
    key: BrandKey,
    assets_base: &str,
    surreal: &store::surreal::SurrealDb,
    pg: &store::postgres::PostgresDb,
) -> Result<Option<RuntimeLetterhead>, LetterheadError> {
    let Some(resolved) = brand_letterheads::for_brand_key(surreal, pg, key.as_str())
        .await
        .map_err(|error| LetterheadError::Store(error.to_string()))?
    else {
        return Ok(None);
    };
    let (mark, warnings) = match resolved.mark {
        StoreMark::Wordmark => (pdf::LetterheadMark::Wordmark, Vec::new()),
        StoreMark::Logo => {
            match fetch_png(assets_base, resolved.logo_object_key.as_deref()).await {
                Ok(bytes) => (pdf::LetterheadMark::Png(bytes), Vec::new()),
                Err(reason) => (
                    pdf::LetterheadMark::Wordmark,
                    vec![format!(
                        "the Brand's uploaded mark could not be drawn ({reason}); the letter \
                         opens on its wordmark"
                    )],
                ),
            }
        }
    };
    Ok(Some(RuntimeLetterhead {
        revision: resolved.updated_at.clone(),
        font_object_key: resolved.font_object_key.clone(),
        letterhead: runtime_frame(&resolved, mark),
        warnings,
    }))
}

/// The frame a resolved runtime row maps onto: every render line is the
/// row's own resolved line, and the mark is the row's choice. The store's
/// `resolve` has already applied its fallbacks (wordmark → the Brand's
/// name, mailbox → `contact@` the site's registrable domain); the frame
/// carries them through untouched.
fn runtime_frame(resolved: &BrandLetterhead, mark: pdf::LetterheadMark) -> pdf::Letterhead {
    pdf::Letterhead {
        name: resolved.wordmark.clone(),
        entity: resolved.firm_legal_name.clone().unwrap_or_default(),
        phone: resolved.phone.clone().unwrap_or_default(),
        email: resolved.email.clone().unwrap_or_default(),
        web: resolved.web.clone().unwrap_or_default(),
        postal_address: resolved.postal_address.clone().unwrap_or_default(),
        font_family: resolved.font_family.clone().unwrap_or_default(),
        mark,
    }
}

/// A Brand's uploaded mark, fetched from the deployment's public assets
/// route — the same route the site itself draws the mark from. The message
/// names the object so a missing upload is diagnosable from the warning.
async fn fetch_png(base: &str, object_key: Option<&str>) -> Result<Vec<u8>, String> {
    let Some(key) = object_key.map(str::trim).filter(|key| !key.is_empty()) else {
        return Err("the Brand has no uploaded mark object".to_string());
    };
    let url = format!("{}/assets/{key}", base.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| format!("GET {url}: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GET {url}: {status}"));
    }
    response
        .bytes()
        .await
        .map(Into::into)
        .map_err(|error| format!("GET {url}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{for_brand, for_project, resolve_brand, LetterheadError};
    use views::brand::BrandKey;

    #[test]
    fn a_declared_brand_cannot_override_the_host() {
        let other = BrandKey::ALL[1];
        assert!(
            resolve_brand(Some(other.as_str()), Some(BrandKey::Neon.hosts()[0])).is_err(),
            "a declaration must agree with the host-selected Brand"
        );
    }

    #[test]
    fn a_declared_brand_requires_a_known_host() {
        let brand = Some(BrandKey::Neon.as_str());
        assert_eq!(resolve_brand(brand, None), Err(LetterheadError::Undeclared));
        assert_eq!(
            resolve_brand(brand, Some("unknown.example.test")),
            Err(LetterheadError::UnknownHost("unknown.example.test".into()))
        );
    }

    #[test]
    fn a_matching_declaration_asserts_the_hosts_brand() {
        for key in BrandKey::ALL.iter().copied() {
            assert_eq!(
                resolve_brand(Some(key.as_str()), Some(key.hosts()[0])),
                Ok(key)
            );
        }
    }

    #[test]
    fn every_registered_host_and_apex_resolves_to_its_brand() {
        for key in BrandKey::ALL.iter().copied() {
            for host in key.hosts() {
                assert_eq!(resolve_brand(None, Some(host)), Ok(key), "{host}");
                assert_eq!(
                    resolve_brand(None, Some(&host.to_uppercase())),
                    Ok(key),
                    "{host}"
                );
            }
            if let Some(apex) = key.apex() {
                assert_eq!(resolve_brand(None, Some(apex)), Ok(key), "{apex}");
            }
        }
    }

    #[test]
    fn an_unknown_brand_host_or_neither_is_refused() {
        assert_eq!(
            for_project(Some("sample-brand"), None),
            Err(LetterheadError::UnknownBrand("sample-brand".to_string()))
        );
        let refused = for_project(None, Some("navigator.example.test")).unwrap_err();
        assert!(refused.to_string().contains("navigator.example.test"));
        assert_eq!(
            for_project(Some(" "), Some("")),
            Err(LetterheadError::Undeclared)
        );
    }

    #[test]
    fn only_the_default_brand_draws_the_compiled_mark() {
        for key in BrandKey::ALL.iter().copied() {
            let letterhead = for_brand(key).expect("every compiled Brand has a letterhead");
            let expected_mark = if key == BrandKey::Neon {
                pdf::LetterheadMark::Compiled
            } else {
                pdf::LetterheadMark::Wordmark
            };
            assert_eq!(letterhead.mark, expected_mark, "{}", key.as_str());
            assert!(letterhead.email.starts_with("contact@"));
            assert!(
                !letterhead.entity.is_empty(),
                "{} names its Firm",
                key.as_str()
            );
        }
    }

    #[tokio::test]
    async fn the_mark_fetch_serves_the_uploaded_bytes() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/assets/brands/vesta/mark.png"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_bytes(b"png-bytes".to_vec()),
            )
            .mount(&server)
            .await;
        let bytes = super::fetch_png(&server.uri(), Some("brands/vesta/mark.png"))
            .await
            .expect("the mark fetches");
        assert_eq!(bytes, b"png-bytes");
    }

    #[tokio::test]
    async fn the_mark_fetch_names_the_url_on_a_miss() {
        let server = wiremock::MockServer::start().await;
        let reason = super::fetch_png(&server.uri(), Some("brands/vesta/mark.png"))
            .await
            .expect_err("a miss is an error");
        assert!(reason.contains("/assets/brands/vesta/mark.png"), "{reason}");
        assert!(reason.contains("404"), "{reason}");
    }

    #[tokio::test]
    async fn the_mark_fetch_refuses_an_unset_object() {
        assert!(super::fetch_png("http://127.0.0.1:1", None).await.is_err());
    }
}
