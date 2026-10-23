//! The approved email-domain policy, derived from each Brand's website URL.
//!
//! A [`crate::brands::Brand`] row may carry a canonical website URL. The set of
//! email domains approved for account admission is derived from those URLs
//! and nothing else: there is no second, hand-kept allowlist. This approval is
//! separate from host routing (the compiled `views::brand::BrandKey`
//! registry), from Brand ownership, and from Navigator role grants.
//!
//! # Rules
//!
//! - **Canonical website URL:** `https://` and an ASCII host only. The host is
//!   lowercased and IDNA-encoded (`bücher.example` → `xn--bcher-kva.example`).
//!   The default port 443 is dropped; any other port is refused. User info, a
//!   path other than `/`, a query, and a fragment are refused. An IP address, a
//!   single-label host, and a trailing dot are refused. The stored form is
//!   `https://<host>`.
//! - **www and apex:** a host whose first label is `www` approves the domain
//!   after it, so `https://www.example.com` and `https://example.com` both
//!   approve `example.com`. No other label is stripped.
//! - **Subdomains:** a website on `brand.example.com` approves exactly
//!   `brand.example.com`. A domain approves neither its parent nor its
//!   subdomains, and comparison is exact, so `mail.example.com` and
//!   `notexample.com` are not admitted by `example.com`.
//! - **Fail closed:** a Brand with no URL, or a stored URL that is not already
//!   canonical, contributes nothing. With no approved domain the policy admits
//!   no one.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use url::{Host, Url};

/// Why a website URL is not a canonical Brand website.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WebsiteUrlError {
    #[error("the website URL is not a URL")]
    Unparseable,
    #[error("the website URL must use https")]
    NotHttps,
    #[error("the website URL must not carry a user name or password")]
    UserInfo,
    #[error("the website URL must not name a port")]
    Port,
    #[error("the website URL must be the site's root, with no path, query, or fragment")]
    NotRoot,
    #[error("the website URL must name a registered domain, not an address or a single label")]
    Host,
}

/// The canonical `https://<host>` form of a Brand website URL.
///
/// # Errors
///
/// The input breaks one of the module's rules.
pub fn canonical_website_url(input: &str) -> Result<String, WebsiteUrlError> {
    let url = Url::parse(input.trim()).map_err(|_| WebsiteUrlError::Unparseable)?;
    if url.scheme() != "https" {
        return Err(WebsiteUrlError::NotHttps);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(WebsiteUrlError::UserInfo);
    }
    if url.port().is_some() {
        return Err(WebsiteUrlError::Port);
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(WebsiteUrlError::NotRoot);
    }
    match url.host() {
        Some(Host::Domain(host)) if is_registered_domain(host) => Ok(format!("https://{host}")),
        _ => Err(WebsiteUrlError::Host),
    }
}

/// At least two non-empty labels, no trailing dot, within DNS length.
fn is_registered_domain(host: &str) -> bool {
    host.len() <= 253
        && host.split('.').count() >= 2
        && host
            .split('.')
            .all(|label| !label.is_empty() && label.len() <= 63)
}

/// The email domain a canonical website URL approves.
fn approved_domain(canonical: &str) -> Option<String> {
    let host = canonical.strip_prefix("https://")?;
    let domain = match host.strip_prefix("www.") {
        Some(apex) if is_registered_domain(apex) => apex,
        _ => host,
    };
    Some(domain.to_string())
}

/// The lowercase, IDNA-encoded domain of an email address, or `None` when it
/// is not a plain `local@domain` address.
fn email_domain(email: &str) -> Option<String> {
    let (local, domain) = email.trim().split_once('@')?;
    if local.is_empty()
        || domain.contains('@')
        || domain
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '%' | '/' | '\\' | ':' | '[' | ']'))
    {
        return None;
    }
    match Host::parse(domain).ok()? {
        Host::Domain(domain) if is_registered_domain(&domain) => Some(domain),
        _ => None,
    }
}

/// The approved email domains, derived from Brand website URLs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainPolicy {
    domains: BTreeSet<String>,
}

/// What changed between two policies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyChange {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl DomainPolicy {
    /// Derive the policy from stored website URLs. A missing or
    /// non-canonical value contributes nothing.
    pub fn from_website_urls<'a>(urls: impl IntoIterator<Item = Option<&'a str>>) -> Self {
        let domains = urls
            .into_iter()
            .flatten()
            .filter(|url| canonical_website_url(url).is_ok_and(|canonical| canonical == *url))
            .filter_map(approved_domain)
            .collect();
        Self { domains }
    }

    /// Derive the policy from Brand rows.
    #[must_use]
    pub fn from_brands(brands: &[crate::brands::Brand]) -> Self {
        Self::from_website_urls(brands.iter().map(|brand| brand.website_url.as_deref()))
    }

    /// The approved domains, sorted.
    pub fn domains(&self) -> impl Iterator<Item = &str> {
        self.domains.iter().map(String::as_str)
    }

    /// Whether no domain is approved, so no one is admitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.domains.is_empty()
    }

    /// A stable digest of the approved set: equal policies share a version,
    /// and any addition or removal changes it.
    #[must_use]
    pub fn version(&self) -> String {
        let mut hasher = Sha256::new();
        for domain in &self.domains {
            hasher.update(domain.as_bytes());
            hasher.update(b"\n");
        }
        hasher
            .finalize()
            .iter()
            .fold(String::with_capacity(64), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            })
    }

    /// Whether `email`'s domain is exactly an approved domain.
    #[must_use]
    pub fn admits(&self, email: &str) -> bool {
        email_domain(email).is_some_and(|domain| self.domains.contains(&domain))
    }

    /// The domains this policy adds to and removes from `previous`.
    #[must_use]
    pub fn changes_since(&self, previous: &Self) -> PolicyChange {
        PolicyChange {
            added: self
                .domains
                .difference(&previous.domains)
                .cloned()
                .collect(),
            removed: previous
                .domains
                .difference(&self.domains)
                .cloned()
                .collect(),
        }
    }
}

/// The policy derived from every Brand row in the store.
///
/// # Errors
///
/// The Brand rows could not be read; a caller must then admit no one.
pub async fn current(
    surreal: &crate::surreal::SurrealDb,
) -> Result<DomainPolicy, crate::brands::BrandError> {
    let mut brands = crate::brands::system_wide(surreal).await?;
    brands.extend(crate::brands::all_firm_scoped(surreal).await?);
    Ok(DomainPolicy::from_brands(&brands))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(urls: &[&str]) -> DomainPolicy {
        DomainPolicy::from_website_urls(urls.iter().map(|url| Some(*url)))
    }

    #[test]
    fn a_website_url_is_stored_as_its_https_origin() {
        for (input, canonical) in [
            ("https://example.com", "https://example.com"),
            ("https://example.com/", "https://example.com"),
            ("  https://WWW.Example.COM/  ", "https://www.example.com"),
            ("https://example.com:443/", "https://example.com"),
            ("https://brand.example.test", "https://brand.example.test"),
            ("https://bücher.example", "https://xn--bcher-kva.example"),
        ] {
            assert_eq!(canonical_website_url(input), Ok(canonical.to_string()));
        }
    }

    #[test]
    fn a_website_url_outside_the_rules_is_refused() {
        for (input, error) in [
            ("", WebsiteUrlError::Unparseable),
            ("example.com", WebsiteUrlError::Unparseable),
            ("http://example.com", WebsiteUrlError::NotHttps),
            ("ftp://example.com", WebsiteUrlError::NotHttps),
            ("https://user@example.com", WebsiteUrlError::UserInfo),
            ("https://user:pass@example.com", WebsiteUrlError::UserInfo),
            ("https://example.com:8443", WebsiteUrlError::Port),
            ("https://example.com/about", WebsiteUrlError::NotRoot),
            ("https://example.com/?ref=1", WebsiteUrlError::NotRoot),
            ("https://example.com/#top", WebsiteUrlError::NotRoot),
            ("https://192.0.2.1", WebsiteUrlError::Host),
            ("https://[2001:db8::1]", WebsiteUrlError::Host),
            ("https://localhost", WebsiteUrlError::Host),
            ("https://example.com.", WebsiteUrlError::Host),
        ] {
            assert_eq!(canonical_website_url(input), Err(error), "{input}");
        }
    }

    #[test]
    fn www_and_apex_approve_the_same_domain_and_no_other_label_is_stripped() {
        assert_eq!(
            policy(&["https://www.example.com"]),
            policy(&["https://example.com"])
        );
        let subdomain = policy(&["https://brand.example.test"]);
        assert_eq!(
            subdomain.domains().collect::<Vec<_>>(),
            ["brand.example.test"]
        );
        assert!(subdomain.admits("person@brand.example.test"));
        assert!(!subdomain.admits("person@example.test"));
    }

    #[test]
    fn an_email_is_admitted_only_by_its_exact_domain() {
        let approved = policy(&["https://www.example.com", "https://example.test"]);
        for email in [
            "person@example.com",
            "Person@EXAMPLE.com",
            " person@example.test ",
        ] {
            assert!(approved.admits(email), "{email}");
        }
        for email in [
            "person@mail.example.com",
            "person@notexample.com",
            "person@example.com.evil.test",
            "person@example.co",
            "person@www.example.com",
            "person@example.com.",
            "person@exa%6Dple.com",
            "person@@example.com",
            "@example.com",
            "person@",
            "example.com",
            "person@192.0.2.1",
            "person@[192.0.2.1]",
        ] {
            assert!(!approved.admits(email), "{email}");
        }
    }

    #[test]
    fn an_internationalized_domain_compares_in_its_ascii_form() {
        let approved = policy(&["https://xn--bcher-kva.example"]);
        assert!(approved.admits("person@bücher.example"));
        assert!(approved.admits("person@xn--bcher-kva.example"));
        assert!(!approved.admits("person@bucher.example"));
    }

    #[test]
    fn a_missing_or_non_canonical_url_contributes_nothing() {
        let ignored = DomainPolicy::from_website_urls([
            None,
            Some("https://example.com/"),
            Some("http://example.com"),
            Some("https://Example.com"),
            Some("not a url"),
        ]);
        assert!(ignored.is_empty());
    }

    #[test]
    fn an_empty_policy_admits_no_one() {
        let empty = DomainPolicy::default();
        assert!(empty.is_empty());
        assert!(!empty.admits("person@example.com"));
        assert_eq!(DomainPolicy::from_brands(&[]), empty);
    }

    async fn website_url(db: &crate::surreal::SurrealDb, key: &str) -> Option<String> {
        crate::brands::find_by_key(db, key)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("brand {key} exists"))
            .website_url
    }

    /// Compiled brand rows written before `website_url` existed take their
    /// registry URL once, on the upgrading apply. A runtime Brand gets none,
    /// a value already present is kept, and a URL the Owner clears stays
    /// cleared on every later boot.
    #[tokio::test]
    async fn the_upgrade_backfill_runs_once_from_the_compiled_registry() {
        let db = crate::test_support::mem_surreal().await;
        assert_eq!(website_url(&db, "vesta").await, None);
        db.query(
            "CREATE $brand SET name = 'Synthetic Runtime Brand', brand_key = 'synthetic-runtime', \
             firm_id = $firm, is_law_firm = true, inserted_at = $now, updated_at = $now; \
             UPDATE brand SET website_url = 'https://example.com' WHERE brand_key = 'neon'; \
             UPSERT schema_version:current SET version = 87;",
        )
        .bind(("now", chrono::Utc::now().to_rfc3339()))
        .bind((
            "firm",
            crate::surreal::record_id("firm", uuid::Uuid::now_v7()),
        ))
        .bind((
            "brand",
            crate::surreal::record_id("brand", uuid::Uuid::now_v7()),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .unwrap();

        crate::schema::apply(&db).await.unwrap();
        for key in crate::firms::CLOSED_BRAND_KEYS {
            let expected = if *key == "neon" {
                Some("https://example.com".to_string())
            } else {
                crate::seed::compiled_brand_website_url(key).map(str::to_string)
            };
            assert_eq!(website_url(&db, key).await, expected, "{key}");
        }
        assert_eq!(website_url(&db, "synthetic-runtime").await, None);
        let policy = current(&db).await.unwrap();
        assert_eq!(
            policy.domains().count(),
            crate::firms::CLOSED_BRAND_KEYS.len(),
            "every compiled brand approves one distinct domain"
        );
        assert!(policy.admits("person@example.com"));

        let vesta = crate::brands::find_by_key(&db, "vesta")
            .await
            .unwrap()
            .unwrap();
        crate::brands::set_website_url(&db, crate::persons::Role::Owner, vesta.id, None)
            .await
            .unwrap();
        crate::schema::apply(&db).await.unwrap();
        assert_eq!(website_url(&db, "vesta").await, None);
    }

    #[test]
    fn every_compiled_website_url_is_already_canonical() {
        for key in crate::firms::CLOSED_BRAND_KEYS {
            let url = crate::seed::compiled_brand_website_url(key)
                .unwrap_or_else(|| panic!("{key} has a compiled website URL"));
            assert_eq!(canonical_website_url(url).as_deref(), Ok(url), "{key}");
        }
    }

    #[test]
    fn additions_and_removals_change_the_version_and_are_reported() {
        let before = policy(&["https://example.com", "https://example.test"]);
        let after = policy(&["https://example.test", "https://brand.example.test"]);
        assert_eq!(
            after.changes_since(&before),
            PolicyChange {
                added: vec!["brand.example.test".into()],
                removed: vec!["example.com".into()],
            }
        );
        assert!(!after.admits("person@example.com"));
        assert_ne!(before.version(), after.version());
        assert_eq!(
            before.version(),
            policy(&["https://example.test", "https://www.example.com"]).version()
        );
        assert_eq!(before.version().len(), 64);
        assert_eq!(before.changes_since(&before), PolicyChange::default());
    }
}
