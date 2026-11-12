//! The `scrapers:` section reader lives in `cli::scraper_config`, shared
//! with the `navigator.yaml` lint; this module binds a parsed site to the
//! adapter registry that only `hudson` carries.

pub(crate) use cli::scraper_config::*;

use crate::scraper::adapter::{find_adapter, SiteAdapter};

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
    // A binding that is not a valid site key cannot match any adapter,
    // and `find_adapter` is the one lookup seam for the registry.
    let bound = SiteKey::new(&entry.adapter).ok();
    bound
        .as_ref()
        .and_then(|key| find_adapter(adapters, key))
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

#[cfg(test)]
mod tests {
    use super::{parse_value, resolve_bound_adapter, ConfigError};
    use crate::scraper::adapter::{SiteAdapter, SiteKey};
    use crate::scraper::testing::MockAdapter;

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

    fn parse_section(
        value: Option<&serde_yaml::Value>,
    ) -> Result<super::ScraperConfig, ConfigError> {
        parse_value(value.expect("a section"))
    }
    #[test]
    fn an_unbound_adapter_names_the_binding_and_the_registry() {
        let config = parse_section(Some(&yaml_section(""))).expect("valid");
        let entry = config.find_site(&site()).expect("configured");
        let Err(error) = resolve_bound_adapter(&[], &site(), entry) else {
            panic!("no adapters are registered in this fixture");
        };
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
}
