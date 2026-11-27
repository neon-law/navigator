//! The per-workflow DBOS switches, readable without the `dbos` feature.
//!
//! Every caller of a ported service — `web`, the thin trigger binaries, the
//! worker's own Restate handlers — reads the same strict `true` / `false`
//! switch to choose between the worker's DBOS ingress and the Restate
//! ingress. Those callers do not host the executor, and several crates
//! depend on `workflows` without its `dbos` feature, so the parser lives
//! outside the feature gate. `dbos_engine` re-exports both items so the
//! established `workflows::dbos_engine::flag_enabled` path keeps resolving.

/// Automatic weekly starts, independent of BillingCanary's native registration.
pub const BILLING_CANARY_SCHEDULE_ENABLED_ENV: &str =
    "NAVIGATOR_DBOS_BILLING_CANARY_SCHEDULE_ENABLED";
/// Automatic daily starts, independent of invoice reconciliation's native registration.
pub const RECONCILE_INVOICES_SCHEDULE_ENABLED_ENV: &str =
    "NAVIGATOR_DBOS_RECONCILE_INVOICES_SCHEDULE_ENABLED";

/// A DBOS switch or connection the environment does not complete.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("{0} is required to run DBOS")]
    Missing(&'static str),
    #[error("{0} is not valid for DBOS")]
    Invalid(&'static str),
}

/// Setting the table opts the real production deployment into read-only daily reporting.
pub const GCP_COST_EXPORT_TABLE: &str = "NAVIGATOR_GCP_COST_EXPORT_TABLE";

pub const GCP_COST_READER: &str = "NAVIGATOR_GCP_COST_READER";

pub fn daily_cost_export(
    get: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Option<billing::gcp_cost_daily::ExportTable>> {
    let Some(table) = get(GCP_COST_EXPORT_TABLE).filter(|s| !s.trim().is_empty()) else {
        return Ok(None);
    };
    if get(store::config::NAVIGATOR_ENVIRONMENT).as_deref() != Some("production")
        || store::config::staging_from(&get)?
        || store::sample_matters_from(store::DeploymentEnvironment::Production, &get)?
    {
        return Ok(None);
    }
    let reader = get(GCP_COST_READER).ok_or(SettingsError::Missing(GCP_COST_READER))?;
    billing::gcp_cost_daily::ExportTable::parse(table.trim(), reader.trim()).map(Some)
}

/// Parse one workflow-specific switch with the same strict `true` / `false`
/// contract as the executor switch.
///
/// # Errors
///
/// Active switches refuse any value other than `true` or `false`. Disabled billing
/// selectors always resolve to false, including stale deployment values.
pub fn flag_enabled(name: &'static str, value: Option<&str>) -> Result<bool, SettingsError> {
    if store::disabled_billing::disabled_billing_flag(name) {
        return Ok(false);
    }
    match value.map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(SettingsError::Invalid(name)),
    }
}

/// Resolve an optional automatic timer. Retired billing schedules stay off.
pub fn schedule_enabled(
    name: &'static str,
    get: &impl Fn(&str) -> Option<String>,
) -> Result<bool, SettingsError> {
    if matches!(
        name,
        BILLING_CANARY_SCHEDULE_ENABLED_ENV | RECONCILE_INVOICES_SCHEDULE_ENABLED_ENV
    ) {
        return Ok(false);
    }
    flag_enabled(name, get(name).as_deref())
}

#[cfg(test)]
mod tests {
    use super::{flag_enabled, SettingsError};

    #[test]
    fn billing_timers_and_selectors_cannot_be_reenabled() {
        for value in [None, Some("true"), Some("false"), Some("invalid")] {
            for selection in store::disabled_billing::DISABLED_BILLING {
                assert!(!flag_enabled(selection.flag, value).unwrap());
            }
            for key in [
                super::BILLING_CANARY_SCHEDULE_ENABLED_ENV,
                super::RECONCILE_INVOICES_SCHEDULE_ENABLED_ENV,
            ] {
                assert!(!super::schedule_enabled(key, &|_| value.map(str::to_owned)).unwrap());
            }
        }
    }

    #[test]
    fn flag_enabled_is_strict() {
        assert!(!flag_enabled("FLAG", None).unwrap_or(true));
        assert!(!flag_enabled("FLAG", Some("")).unwrap_or(true));
        assert!(!flag_enabled("FLAG", Some(" false ")).unwrap_or(true));
        assert!(flag_enabled("FLAG", Some("true")).unwrap_or(false));
        assert_eq!(
            flag_enabled("FLAG", Some("yes")),
            Err(SettingsError::Invalid("FLAG"))
        );
    }
}

#[cfg(test)]
mod daily_cost_tests {
    use super::{daily_cost_export, GCP_COST_EXPORT_TABLE, GCP_COST_READER};

    #[test]
    fn only_configured_live_production_selects_the_billing_export() {
        let configured =
            |profile: Option<&str>, staging: &str, simulated: &str, reader: Option<&str>| {
                daily_cost_export(|key| {
                    match key {
                        GCP_COST_EXPORT_TABLE => Some("x.sample.billing"),
                        GCP_COST_READER => reader,
                        store::config::NAVIGATOR_ENVIRONMENT => profile,
                        store::config::NAVIGATOR_STAGING => Some(staging),
                        store::config::NAVIGATOR_SIMULATED_MATTERS => Some(simulated),
                        _ => None,
                    }
                    .map(str::to_owned)
                })
            };
        let reader = Some(concat!("reader@", "x", ".iam.gserviceaccount.com"));
        assert!(configured(Some("production"), "false", "false", reader)
            .unwrap()
            .is_some());
        for (profile, staging, simulated) in [
            (None, "false", "false"),
            (Some("dev"), "false", "false"),
            (Some("production"), "true", "false"),
            (Some("production"), "false", "true"),
        ] {
            assert!(configured(profile, staging, simulated, None)
                .unwrap()
                .is_none());
        }
        assert!(configured(Some("production"), "false", "false", None).is_err());
        assert!(configured(Some("production"), "false", "false", Some("invalid/reader")).is_err());
        assert!(daily_cost_export(|_| None).unwrap().is_none());
    }
}
