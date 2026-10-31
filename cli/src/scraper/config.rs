//! The `scrapers:` section of `navigator.yaml` — the site registry, each
//! site's adapter binding and portal URL, and the non-secret run options.
//! One accepted key set, parsed by one reader the manifest lint and the
//! `scraper` commands share, so a site `scraper sites` honours is a site
//! `scraper run` can resolve. Nothing here is secret: portal logins live
//! in the Firm secrets store, addressed by site key.
//!
//! ```yaml
//! scrapers:
//!   sites:
//!     la-superior:
//!       adapter: la-superior
//!       portal_url: https://canvas.lasuperiorcourt.org
//!   options:
//!     schedule_hint: daily 06:00 UTC
//!     download_root: .navigator/scraper
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use url::Url;

use crate::scraper::adapter::{SiteAdapter, SiteKey};

/// The manifest key the site registry lives under.
pub(crate) const SECTION: &str = "scrapers";

/// Why a `scrapers:` section could not be used. Every message names the
/// key path and the accepted shape, so whoever edits the manifest can fix
/// it without reading this module.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ConfigError {
    #[error("`scrapers:` must be a mapping of `sites` (and optionally `options`)")]
    NotAMapping,
    #[error("`scrapers:` must carry a `sites:` mapping, even an empty one")]
    NoSites,
    #[error("`scrapers.sites:` must be a mapping of site key to site entry")]
    SitesNotAMapping,
    #[error("{0}: unknown key `{1}`; a site entry accepts {2}")]
    UnknownSiteKey(String, String, String),
    #[error("{0}: unknown key `{1}`; `options` accepts {2}")]
    UnknownOptionKey(String, String, String),
    #[error("{0}: unknown key `{1}`; `scrapers:` accepts `sites` and `options`")]
    UnknownSectionKey(String, String),
    #[error("{0}: `{1}` must be a non-empty string")]
    MissingText(String, String),
    #[error("{0}: `portal_url` must be an absolute http(s) URL with a host")]
    BadPortalUrl(String),
    #[error("{0}: the site key is invalid: {1}")]
    BadSiteKey(String, String),
    #[error(
        "no site `{site}` is configured under `scrapers.sites:`; configured sites: {configured}"
    )]
    UnknownSite { site: String, configured: String },
    #[error(
        "`scrapers.sites.{site}`: the portal host `{host}` is not in `allowed_hosts:`; add it with a reason before scraping"
    )]
    HostNotAllowed { site: String, host: String },
    #[error(
        "the `{site}` site binds adapter `{adapter}`, but no adapter with that key is registered; registered adapters: {registered}"
    )]
    UnboundAdapter {
        site: String,
        adapter: String,
        registered: String,
    },
}

/// One configured site: which adapter scrapes it and where the portal
/// lives. The portal URL is the declared, reviewed address — adapters
/// navigate within it, and [`ScraperConfig::check_allowed_hosts`] keeps it
/// inside the Project's `allowed_hosts:` ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SiteEntry {
    /// The adapter registry key bound to this site, e.g. `la-superior`.
    pub adapter: String,
    /// The portal's absolute URL.
    pub portal_url: String,
}

impl SiteEntry {
    /// The portal URL's host — the value `allowed_hosts:` is checked
    /// against.
    ///
    /// # Errors
    ///
    /// When the URL does not parse; parse rejects that at the door, so a
    /// constructed entry always carries a host.
    pub(crate) fn portal_host(&self) -> Result<String, ConfigError> {
        portal_host(&self.portal_url).ok_or_else(|| ConfigError::BadPortalUrl(String::new()))
    }
}

/// The non-secret run options. `schedule_hint` is declarative — when the
/// Firm plans to run this scraper — and surfaces in `scraper sites`;
/// `download_root` relocates the per-site state directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ScraperOptions {
    pub schedule_hint: Option<String>,
    pub download_root: Option<PathBuf>,
}

const SITE_KEYS: &[&str] = &["adapter", "portal_url"];
const OPTION_KEYS: &[&str] = &["download_root", "schedule_hint"];
const SECTION_KEYS: &[&str] = &["options", "sites"];

/// The parsed `scrapers:` section. Sites are keyed by [`SiteKey`] so the
/// registry, the on-disk state directory, and the credential entry all
/// share one normalized identity.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ScraperConfig {
    pub sites: BTreeMap<SiteKey, SiteEntry>,
    pub options: ScraperOptions,
}

impl ScraperConfig {
    /// Look up one configured site.
    ///
    /// # Errors
    ///
    /// When the site is not in the registry; the error lists the
    /// configured sites.
    pub(crate) fn find_site(&self, site: &SiteKey) -> Result<&SiteEntry, ConfigError> {
        let configured = self
            .sites
            .keys()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();
        self.sites
            .get(site)
            .ok_or_else(|| ConfigError::UnknownSite {
                site: site.to_string(),
                configured: if configured.is_empty() {
                    "none".to_owned()
                } else {
                    configured.join(", ")
                },
            })
    }

    /// The target site's portal host must be an `allowed_hosts:` entry —
    /// the manifest's outbound ring is the one place outbound hosts are
    /// reviewed.
    ///
    /// # Errors
    ///
    /// When the site is unconfigured or its host is not in the allowlist.
    pub(crate) fn check_allowed_host(
        &self,
        site: &SiteKey,
        allowed_hosts: &BTreeMap<String, String>,
    ) -> Result<(), ConfigError> {
        let entry = self.find_site(site)?;
        let host = portal_host(&entry.portal_url)
            .ok_or_else(|| ConfigError::BadPortalUrl(format!("scrapers.sites.{site}")))?;
        if !allowed_hosts.contains_key(&host) {
            return Err(ConfigError::HostNotAllowed {
                site: site.to_string(),
                host,
            });
        }
        Ok(())
    }
}

/// Resolve the adapter a configured site binds to, against the registry.
///
/// # Errors
///
/// [`ConfigError::UnboundAdapter`] when no registered adapter carries the
/// bound key — the expected state until a site's adapter lands.
pub(crate) fn resolve_bound_adapter<'a>(
    adapters: &'a [Box<dyn SiteAdapter>],
    site: &SiteKey,
    entry: &SiteEntry,
) -> Result<&'a dyn SiteAdapter, ConfigError> {
    let registered = adapters
        .iter()
        .map(|adapter| adapter.site().to_string())
        .collect::<Vec<_>>();
    adapters
        .iter()
        .find(|adapter| adapter.site().as_str() == entry.adapter)
        .map(|adapter| &**adapter)
        .ok_or_else(|| ConfigError::UnboundAdapter {
            site: site.to_string(),
            adapter: entry.adapter.clone(),
            registered: if registered.is_empty() {
                "none".to_owned()
            } else {
                registered.join(", ")
            },
        })
}

/// Parse the `scrapers:` section out of a parsed `navigator.yaml`
/// document. A missing section is a valid, empty config: a Project with
/// no scrapers configured scrapes nothing.
///
/// # Errors
///
/// Every shape problem in [`ConfigError`]; the message names the key path.
pub(crate) fn parse_section(
    section: Option<&serde_yaml::Value>,
) -> Result<ScraperConfig, ConfigError> {
    let Some(section) = section else {
        return Ok(ScraperConfig::default());
    };
    let Some(mapping) = section.as_mapping() else {
        return Err(ConfigError::NotAMapping);
    };
    for key in mapping.keys() {
        let name = key.as_str().unwrap_or("(non-string key)");
        if !SECTION_KEYS.contains(&name) {
            return Err(ConfigError::UnknownSectionKey(
                SECTION.to_owned(),
                name.to_owned(),
            ));
        }
    }
    let mut sites = BTreeMap::new();
    match mapping.get("sites") {
        None => return Err(ConfigError::NoSites),
        Some(sites_value) => {
            let Some(sites_mapping) = sites_value.as_mapping() else {
                return Err(ConfigError::SitesNotAMapping);
            };
            for (key, entry) in sites_mapping {
                let Some(name) = key.as_str() else {
                    return Err(ConfigError::SitesNotAMapping);
                };
                let path = format!("scrapers.sites.{name}");
                let site_key = SiteKey::new(name)
                    .map_err(|error| ConfigError::BadSiteKey(path.clone(), error.to_string()))?;
                let entry = parse_site_entry(&path, entry)?;
                sites.insert(site_key, entry);
            }
        }
    }
    let options = parse_options(mapping.get("options"))?;
    Ok(ScraperConfig { sites, options })
}

/// Parse a whole raw `scrapers` value the manifest carried. The manifest
/// lint and the scraper commands both enter here, so a shape one side
/// honours cannot be a shape the other refuses.
///
/// # Errors
///
/// See [`parse_section`].
pub(crate) fn parse_value(value: &serde_yaml::Value) -> Result<ScraperConfig, ConfigError> {
    parse_section(Some(value))
}

fn parse_site_entry(path: &str, entry: &serde_yaml::Value) -> Result<SiteEntry, ConfigError> {
    let Some(entry) = entry.as_mapping() else {
        return Err(ConfigError::UnknownSiteKey(
            path.to_owned(),
            "(not a mapping)".to_owned(),
            site_keys_phrase(),
        ));
    };
    for key in entry.keys() {
        let name = key.as_str().unwrap_or("(non-string key)");
        if !SITE_KEYS.contains(&name) {
            return Err(ConfigError::UnknownSiteKey(
                path.to_owned(),
                name.to_owned(),
                site_keys_phrase(),
            ));
        }
    }
    let adapter = text_at(entry, "adapter", path)?;
    let portal_url = text_at(entry, "portal_url", path)?;
    if portal_host(&portal_url).is_none() {
        return Err(ConfigError::BadPortalUrl(path.to_owned()));
    }
    Ok(SiteEntry {
        adapter,
        portal_url,
    })
}

fn parse_options(options: Option<&serde_yaml::Value>) -> Result<ScraperOptions, ConfigError> {
    let Some(options) = options else {
        return Ok(ScraperOptions::default());
    };
    let Some(options) = options.as_mapping() else {
        return Err(ConfigError::UnknownOptionKey(
            "scrapers.options".to_owned(),
            "(not a mapping)".to_owned(),
            option_keys_phrase(),
        ));
    };
    for key in options.keys() {
        let name = key.as_str().unwrap_or("(non-string key)");
        if !OPTION_KEYS.contains(&name) {
            return Err(ConfigError::UnknownOptionKey(
                "scrapers.options".to_owned(),
                name.to_owned(),
                option_keys_phrase(),
            ));
        }
    }
    let schedule_hint = optional_text(options, "schedule_hint")?;
    let download_root = optional_text(options, "download_root")?.map(PathBuf::from);
    Ok(ScraperOptions {
        schedule_hint,
        download_root,
    })
}

fn optional_text(mapping: &serde_yaml::Mapping, key: &str) -> Result<Option<String>, ConfigError> {
    match mapping.get(key) {
        None => Ok(None),
        Some(value) => scalar_string(value)
            .filter(|text| !text.trim().is_empty())
            .map(Some)
            .ok_or_else(|| {
                ConfigError::MissingText(format!("scrapers.options.{key}"), key.to_owned())
            }),
    }
}

fn text_at(mapping: &serde_yaml::Mapping, key: &str, path: &str) -> Result<String, ConfigError> {
    let field = format!("{path}.{key}");
    match mapping.get(key) {
        Some(value) => scalar_string(value)
            .filter(|text| !text.trim().is_empty())
            .ok_or(ConfigError::MissingText(field.clone(), key.to_owned())),
        None => Err(ConfigError::MissingText(field, key.to_owned())),
    }
}

fn scalar_string(value: &serde_yaml::Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn portal_url(url: &str) -> Option<Url> {
    let parsed = Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then_some(parsed)
}

fn portal_host(url: &str) -> Option<String> {
    portal_url(url)?.host_str().map(str::to_owned)
}

fn site_keys_phrase() -> String {
    SITE_KEYS
        .iter()
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>()
        .join(" and ")
}

fn option_keys_phrase() -> String {
    OPTION_KEYS
        .iter()
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Where per-site state (staging, documents, run state) lives when the
/// config sets no `download_root`: under the Project checkout's
/// `.navigator/scraper`.
#[must_use]
pub(crate) fn default_state_root(project_root: &Path) -> PathBuf {
    project_root.join(".navigator").join("scraper")
}

#[cfg(test)]
mod tests {
    use super::{parse_section, parse_value, resolve_bound_adapter, ConfigError, ScraperConfig};
    use crate::scraper::adapter::{SiteAdapter, SiteKey};
    use crate::scraper::testing::MockAdapter;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn yaml_section(extra: &str) -> serde_yaml::Value {
        let text = format!(
            "sites:\n  la-superior:\n    adapter: la-superior\n    \
             portal_url: https://canvas.lasuperiorcourt.org\n{extra}"
        );
        serde_yaml::from_str(&text).expect("valid yaml")
    }

    fn site() -> SiteKey {
        SiteKey::new("la-superior").expect("valid key")
    }

    #[test]
    fn a_full_section_parses() {
        let extra = "options:\n  schedule_hint: daily 06:00 UTC\n  download_root: .state/scraper\n";
        let config = parse_section(Some(&yaml_section(extra))).expect("a valid section");
        assert_eq!(config.sites.len(), 1);
        let entry = config.find_site(&site()).expect("configured");
        assert_eq!(entry.adapter, "la-superior");
        assert_eq!(entry.portal_url, "https://canvas.lasuperiorcourt.org");
        assert_eq!(
            config.options.schedule_hint.as_deref(),
            Some("daily 06:00 UTC")
        );
        assert_eq!(
            config.options.download_root,
            Some(PathBuf::from(".state/scraper"))
        );
    }

    #[test]
    fn a_bare_sites_section_parses_with_default_options() {
        let config = parse_section(Some(&yaml_section(""))).expect("a valid section");
        assert_eq!(config.options, super::ScraperOptions::default());
    }

    #[test]
    fn a_missing_section_is_an_empty_config() {
        let config = parse_section(None).expect("no section is valid");
        assert!(config.sites.is_empty());
        assert_eq!(config.options, super::ScraperOptions::default());
    }

    #[test]
    fn a_null_section_is_an_empty_config() {
        let value = serde_yaml::from_str::<serde_yaml::Value>("null").expect("null");
        let config = parse_section(Some(&value)).expect("a null section is absent");
        assert!(config.sites.is_empty());
    }

    #[test]
    fn a_non_mapping_section_is_rejected() {
        let value = serde_yaml::from_str::<serde_yaml::Value>("[1]").expect("valid yaml");
        let error = parse_section(Some(&value)).expect_err("rejected");
        assert_eq!(error, ConfigError::NotAMapping);
    }

    #[test]
    fn a_section_without_sites_is_rejected() {
        let value = serde_yaml::from_str::<serde_yaml::Value>("options: {}").expect("valid yaml");
        let error = parse_section(Some(&value)).expect_err("rejected");
        assert!(error.to_string().contains("must carry a `sites:` mapping"));
    }

    #[test]
    fn a_site_entry_missing_its_adapter_is_rejected() {
        let value = serde_yaml::from_str::<serde_yaml::Value>(
            "sites:\n  la-superior:\n    portal_url: https://portal.example",
        )
        .expect("valid yaml");
        let error = parse_section(Some(&value)).expect_err("rejected");
        assert!(error
            .to_string()
            .contains("scrapers.sites.la-superior.adapter"));
    }

    #[test]
    fn a_site_entry_without_a_host_is_rejected() {
        let value = serde_yaml::from_str::<serde_yaml::Value>(
            "sites:\n  la-superior:\n    adapter: la-superior\n    portal_url: not a url\n",
        )
        .expect("valid yaml");
        let error = parse_section(Some(&value)).expect_err("rejected");
        assert!(error.to_string().contains("absolute http(s) URL"));
    }

    #[test]
    fn a_site_entry_with_an_unknown_key_is_rejected() {
        let value = serde_yaml::from_str::<serde_yaml::Value>(
            "sites:\n  la-superior:\n    adapter: a\n    portal_url: https://portal.example\n    polling: fast\n",
        )
        .expect("valid yaml");
        let error = parse_section(Some(&value)).expect_err("rejected");
        assert!(error.to_string().contains("unknown key `polling`"));
    }

    #[test]
    fn an_invalid_site_key_is_rejected_by_name() {
        let value = serde_yaml::from_str::<serde_yaml::Value>(
            "sites:\n  LA SUPERIOR:\n    adapter: a\n    portal_url: https://portal.example\n",
        )
        .expect("valid yaml");
        let error = parse_section(Some(&value)).expect_err("rejected");
        assert!(
            matches!(error, ConfigError::BadSiteKey(ref path, _) if path == "scrapers.sites.LA SUPERIOR")
        );
    }

    #[test]
    fn an_unknown_options_key_is_rejected() {
        let value = serde_yaml::from_str::<serde_yaml::Value>(
            "sites: {}\noptions:\n  interval_minutes: 30\n",
        )
        .expect("valid yaml");
        let error = parse_section(Some(&value)).expect_err("rejected");
        assert!(error.to_string().contains("unknown key `interval_minutes`"));
    }

    #[test]
    fn an_unknown_section_key_is_rejected() {
        let value =
            serde_yaml::from_str::<serde_yaml::Value>("sites: {}\nschedules: []").expect("valid");
        let error = parse_section(Some(&value)).expect_err("rejected");
        assert!(error.to_string().contains("unknown key `schedules`"));
    }

    #[test]
    fn an_empty_sites_map_is_valid_but_resolves_nothing() {
        let value = serde_yaml::from_str::<serde_yaml::Value>("sites: {}").expect("valid yaml");
        let config = parse_section(Some(&value)).expect("valid");
        let error = config
            .find_site(&site())
            .expect_err("nothing is configured");
        assert!(
            error.to_string().contains("configured sites: none"),
            "the error says the registry is empty: {error}"
        );
    }

    #[test]
    fn an_unknown_site_lists_the_configured_sites() {
        let config = parse_section(Some(&yaml_section(""))).expect("valid");
        let pacer = SiteKey::new("pacer").expect("valid key");
        let error = config.find_site(&pacer).expect_err("not configured");
        assert!(
            error.to_string().contains("configured sites: la-superior"),
            "the error lists what is configured: {error}"
        );
    }

    #[test]
    fn a_portal_host_outside_allowed_hosts_is_rejected() {
        let config = parse_section(Some(&yaml_section(""))).expect("valid");
        let mut allowed = BTreeMap::new();
        allowed.insert("pacer.example".to_owned(), "wrong court".to_owned());
        let error = config
            .check_allowed_host(&site(), &allowed)
            .expect_err("the host is not allowed");
        assert!(
            error.to_string().contains("canvas.lasuperiorcourt.org"),
            "the error names the host to add: {error}"
        );
        assert!(error.to_string().contains("`allowed_hosts:`"));
    }

    #[test]
    fn a_portal_host_inside_allowed_hosts_passes() {
        let config = parse_section(Some(&yaml_section(""))).expect("valid");
        let mut allowed = BTreeMap::new();
        allowed.insert(
            "canvas.lasuperiorcourt.org".to_owned(),
            "La Superior canvas".to_owned(),
        );
        config
            .check_allowed_host(&site(), &allowed)
            .expect("the host is allowed");
    }

    #[test]
    fn an_unbound_adapter_names_the_binding_and_the_registry() {
        let config = parse_section(Some(&yaml_section(""))).expect("valid");
        let entry = config.find_site(&site()).expect("configured");
        let error = resolve_bound_adapter(&[], &site(), entry).expect_err("no adapters registered");
        assert!(
            error.to_string().contains("binds adapter `la-superior`"),
            "the error names the binding: {error}"
        );
        assert!(
            error.to_string().contains("registered adapters: none"),
            "the error names the empty registry: {error}"
        );
    }

    #[test]
    fn a_bound_adapter_resolves_from_the_registry() {
        let config = parse_section(Some(&yaml_section(""))).expect("valid");
        let entry = config.find_site(&site()).expect("configured");
        let adapters: Vec<Box<dyn SiteAdapter>> = vec![Box::new(MockAdapter::simple(site()))];
        let resolved = resolve_bound_adapter(&adapters, &site(), entry).expect("bound");
        assert_eq!(resolved.site(), &site());
    }

    #[test]
    fn an_entry_host_round_trips_through_the_url() {
        let config = parse_section(Some(&yaml_section(""))).expect("valid");
        let entry = config.find_site(&site()).expect("configured");
        assert_eq!(
            entry.portal_host().expect("a host"),
            "canvas.lasuperiorcourt.org"
        );
    }

    #[test]
    fn parse_value_delegates_to_the_section_reader() {
        let value = yaml_section("");
        assert_eq!(
            parse_value(&value).expect("valid"),
            parse_section(Some(&value)).expect("valid")
        );
    }

    #[test]
    fn the_default_state_root_sits_under_the_checkout() {
        assert_eq!(
            super::default_state_root(std::path::Path::new("/proj")),
            PathBuf::from("/proj/.navigator/scraper")
        );
    }
}
