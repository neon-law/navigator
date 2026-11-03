//! The letterhead a Project's letter goes out under, resolved from the
//! Project's Brand (LAW-265).
//!
//! `navigator.yaml` names the Brand two ways. `project.brand` is the Brand
//! key itself and wins when declared; otherwise `project.host` is looked up
//! in the compiled registry ([`views::brand::registered_brand_key`], or the
//! Brand's apex). The resolved Brand frames the letter: its display name as
//! the wordmark, its Firm's legal name beneath it, its production host on
//! the web line, `contact@` that host's registrable domain, and its phone.
//!
//! A Brand the registry does not know is a refusal naming the key or host,
//! never the default letterhead: a letter on the wrong Brand's letterhead
//! tells its reader the wrong firm wrote it.

use std::fmt;

use views::brand::{brand_key_for_apex, registered_brand_key, BrandKey, DEFAULT_BRANDING};

/// Why a Project's letter has no letterhead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LetterheadError {
    /// `project.brand` names no compiled Brand.
    UnknownBrand(String),
    /// `project.host` maps to no compiled Brand.
    UnknownHost(String),
    /// `navigator.yaml` names neither a Brand nor a host.
    Undeclared,
    /// The compiled Brand's host cannot name a mailbox domain.
    InvalidBrandHost(BrandKey),
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
                 letterhead; correct the host or declare `project.brand`"
            ),
            Self::Undeclared => write!(
                f,
                "navigator.yaml declares neither `project.brand` nor `project.host`, so the \
                 letter has no letterhead"
            ),
            Self::InvalidBrandHost(key) => write!(
                f,
                "Brand `{}` has no host a letterhead mailbox can be built on",
                key.as_str()
            ),
        }
    }
}

impl std::error::Error for LetterheadError {}

/// The Brand a Project's `navigator.yaml` names: `brand` wins, else `host`.
///
/// # Errors
/// [`LetterheadError`] naming the key or host that resolves to no Brand.
pub fn resolve_brand(brand: Option<&str>, host: Option<&str>) -> Result<BrandKey, LetterheadError> {
    fn declared(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|v| !v.is_empty())
    }
    if let Some(key) = declared(brand) {
        return BrandKey::parse(key).ok_or_else(|| LetterheadError::UnknownBrand(key.to_string()));
    }
    let host = declared(host).ok_or(LetterheadError::Undeclared)?;
    registered_brand_key(host)
        .or_else(|| brand_key_for_apex(host))
        .ok_or_else(|| LetterheadError::UnknownHost(host.to_string()))
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
    letterhead.logo = key == BrandKey::Neon;
    Ok(letterhead)
}

/// The letterhead for a Project declaring `brand` and `host`.
///
/// # Errors
/// [`LetterheadError`] when neither resolves to a Brand.
pub fn for_project(
    brand: Option<&str>,
    host: Option<&str>,
) -> Result<pdf::Letterhead, LetterheadError> {
    for_brand(resolve_brand(brand, host)?)
}

#[cfg(test)]
mod tests {
    use super::{for_brand, for_project, resolve_brand, LetterheadError};
    use views::brand::BrandKey;

    #[test]
    fn a_declared_brand_wins_over_the_host() {
        let other = BrandKey::ALL[1];
        assert_eq!(
            resolve_brand(Some(other.as_str()), Some(BrandKey::Neon.hosts()[0])),
            Ok(other)
        );
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
            assert_eq!(letterhead.logo, key == BrandKey::Neon, "{}", key.as_str());
            assert!(letterhead.email.starts_with("contact@"));
            assert!(
                !letterhead.entity.is_empty(),
                "{} names its Firm",
                key.as_str()
            );
        }
    }
}
