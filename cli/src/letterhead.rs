//! Which Brand a Project's letter goes out under (LAW-265, LAW-266).
//!
//! `project.host` in `navigator.yaml` selects the Brand from the compiled
//! registry ([`views::brand::registered_brand_key`], or the Brand's apex).
//! An optional `project.brand` must agree with that host-selected Brand.
//! `navigator-deploy` renders the letterhead frame for the Brand this resolves.
//!
//! A Brand the registry does not know is a refusal naming the key or host,
//! never the default letterhead: a letter on the wrong Brand's letterhead
//! tells its reader the wrong firm wrote it.

use std::fmt;

use views::brand::{brand_key_for_apex, registered_brand_key, BrandKey};

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

#[cfg(test)]
mod tests {
    use super::{resolve_brand, LetterheadError};
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
}
